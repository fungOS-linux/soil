//! Minimal Confluence ingress.
//!
//! A small host process accepts an external webhook, verifies its signature,
//! deduplicates the delivery, and publishes one Unibus fact. The Unibus names
//! are the stable contract; Elmers and other consumers do not depend on this
//! process.

use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, endpoint::presets};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use unibus::client::ClientConfig;
use unibus_sdk::{Publish, Session};

pub mod spore;

pub const EVENT_KIND: &str = "confluence.webhook.received.v1";
pub const MAX_BODY: usize = 64 * 1024;
pub const IROH_ALPN: &[u8] = b"fungos/confluence/webhook/1";

/// The durable fact published to Unibus. Consumers must treat this as
/// at-least-once and deduplicate on `provider` + `delivery_id`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfluenceEvent {
    pub tenant: String,
    pub provider: String,
    pub delivery_id: String,
    pub event_type: String,
    pub received_at_ms: u64,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrohRequest {
    pub provider: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestResponse {
    pub status: u16,
    pub request_id: Option<String>,
    pub duplicate: bool,
}

pub trait Publisher: Send + Sync {
    fn publish(&self, event: &ConfluenceEvent) -> Result<String>;
}

pub struct UnibusPublisher {
    session: Mutex<Session>,
}

impl UnibusPublisher {
    pub fn connect(config: ClientConfig) -> Result<Self> {
        Ok(Self {
            session: Mutex::new(Session::connect(config)?),
        })
    }

    /// Connect by trying `endpoints` in order, cloning `base` with each
    /// `hub_endpoint` in turn. See [`soil_wasi_host::UnibusBus::connect_any`]
    /// for the fleet guidance; the same ordered failover applies here for the
    /// native `serve` path so pris/batty hosts survive a single hub outage.
    pub fn connect_any(base: &ClientConfig, endpoints: &[String]) -> Result<Self> {
        anyhow::ensure!(
            !endpoints.is_empty(),
            "at least one hub endpoint is required"
        );
        let mut last_error = None;
        for endpoint in endpoints {
            let mut config = base.clone();
            config.hub_endpoint = endpoint.clone();
            match Session::connect(config) {
                Ok(session) => {
                    return Ok(Self {
                        session: Mutex::new(session),
                    });
                }
                Err(error) => {
                    tracing::warn!(hub = %endpoint, "unibus hub unavailable, trying next: {error:#}");
                    last_error = Some(error);
                }
            }
        }
        let detail = last_error
            .map(|error| format!("{error:#}"))
            .unwrap_or_else(|| "no hub was tried".to_string());
        anyhow::bail!(
            "all unibus hubs unreachable ({}): {detail}",
            endpoints.join(", ")
        )
    }
}

impl Publisher for UnibusPublisher {
    fn publish(&self, event: &ConfluenceEvent) -> Result<String> {
        let body = serde_json::to_value(event).context("encode confluence event")?;
        let publish = Publish {
            destination: None,
            kind: EVENT_KIND.to_string(),
            body,
        };
        self.session
            .lock()
            .map_err(|_| anyhow::anyhow!("unibus session lock poisoned"))?
            .publish(publish)
    }
}

#[derive(Clone)]
pub struct ConfluenceConfig {
    pub tenant: String,
    pub webhook_secret: Option<Vec<u8>>,
    pub ingress_token: Option<String>,
    pub allow_unsigned: bool,
}

#[derive(Clone)]
pub struct AppState {
    config: ConfluenceConfig,
    publisher: Arc<dyn Publisher>,
    seen: Arc<Mutex<HashMap<String, String>>>,
}

impl AppState {
    pub fn new(config: ConfluenceConfig, publisher: Arc<dyn Publisher>) -> Self {
        Self {
            config,
            publisher,
            seen: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn config(&self) -> &ConfluenceConfig {
        &self.config
    }

    /// Shared by the HTTP and Iroh listeners. Iroh has already authenticated
    /// the peer and therefore does not replay an HTTP signature check.
    pub fn ingest(
        &self,
        provider: &str,
        headers: &HeaderMap,
        body: &[u8],
        authenticated: bool,
    ) -> IngestResponse {
        if body.len() > MAX_BODY {
            return IngestResponse {
                status: 413,
                request_id: None,
                duplicate: false,
            };
        }
        if !authenticated && !self.authorize(headers, body) {
            return IngestResponse {
                status: 401,
                request_id: None,
                duplicate: false,
            };
        }
        let body: Value = match serde_json::from_slice(body) {
            Ok(value @ Value::Object(_)) => value,
            Ok(_) => {
                return IngestResponse {
                    status: 415,
                    request_id: None,
                    duplicate: false,
                };
            }
            Err(_) => {
                return IngestResponse {
                    status: 400,
                    request_id: None,
                    duplicate: false,
                };
            }
        };
        let delivery_id = delivery_id(headers, &body);
        let event_type = header(headers, "x-event-type")
            .or_else(|| header(headers, "x-github-event"))
            .unwrap_or("webhook")
            .to_string();
        let event = ConfluenceEvent {
            tenant: self.config.tenant.clone(),
            provider: provider.to_string(),
            delivery_id: delivery_id.clone(),
            event_type,
            received_at_ms: now_ms(),
            headers: safe_headers(headers),
            body,
        };
        let key = format!("{}:{}", event.provider, event.delivery_id);
        if let Ok(seen) = self.seen.lock()
            && let Some(request_id) = seen.get(&key)
        {
            return IngestResponse {
                status: 200,
                request_id: Some(request_id.clone()),
                duplicate: true,
            };
        }
        match self.publisher.publish(&event) {
            Ok(request_id) => {
                if let Ok(mut seen) = self.seen.lock() {
                    if seen.len() >= 10_000 {
                        seen.clear();
                    }
                    seen.insert(key, request_id.clone());
                }
                IngestResponse {
                    status: 202,
                    request_id: Some(request_id),
                    duplicate: false,
                }
            }
            Err(error) => {
                tracing::warn!(%error, provider = %event.provider, "confluence publish failed");
                IngestResponse {
                    status: 503,
                    request_id: None,
                    duplicate: false,
                }
            }
        }
    }

    fn authorize(&self, headers: &HeaderMap, body: &[u8]) -> bool {
        if let Some(secret) = &self.config.webhook_secret {
            let Ok(webhook) = standardwebhooks::Webhook::from_bytes(secret.clone()) else {
                return false;
            };
            return webhook.verify(body, headers).is_ok();
        }
        if let Some(token) = &self.config.ingress_token {
            let expected = format!("Bearer {token}");
            let actual = header(headers, "authorization").unwrap_or("");
            return bool::from(actual.as_bytes().ct_eq(expected.as_bytes()));
        }
        self.config.allow_unsigned
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/webhooks/{provider}", post(webhook))
        .layer(DefaultBodyLimit::max(MAX_BODY + 1))
        .with_state(state)
}

#[derive(Clone)]
struct RelayState {
    endpoint_id: EndpointId,
    address: SocketAddr,
}

/// Local HTTP-to-Iroh edge used when the caller cannot speak Iroh itself.
/// A Cloudflare Worker, for example, can POST here; this process forwards the
/// same body over the pinned Iroh endpoint.
pub fn relay_router(endpoint_id: EndpointId, address: SocketAddr) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"ok": true, "role": "relay"})) }),
        )
        .route("/webhooks/{provider}", post(relay_webhook))
        .layer(DefaultBodyLimit::max(MAX_BODY + 1))
        .with_state(Arc::new(RelayState {
            endpoint_id,
            address,
        }))
}

async fn relay_webhook(
    State(state): State<Arc<RelayState>>,
    Path(provider): Path<String>,
    body: Bytes,
) -> Response {
    let body: Value = match serde_json::from_slice(&body) {
        Ok(value @ Value::Object(_)) => value,
        Ok(_) => {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "webhook body must be an object",
            )
                .into_response();
        }
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid webhook JSON").into_response(),
    };
    match push_iroh(state.endpoint_id, state.address, provider, body).await {
        Ok(response) => {
            let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::BAD_GATEWAY);
            (status, Json(response)).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "confluence iroh relay failed");
            (StatusCode::BAD_GATEWAY, "iroh relay failed").into_response()
        }
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "kind": EVENT_KIND,
        "tenant": state.config.tenant,
    }))
}

async fn webhook(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let response = state.ingest(&provider, &headers, &body, false);
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::BAD_REQUEST);
    (status, Json(response)).into_response()
}

pub async fn bind_iroh(
    state: Arc<AppState>,
    secret_key: SecretKey,
    bind_addr: SocketAddr,
    allowed_peers: Vec<EndpointId>,
    allow_any: bool,
) -> Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr(bind_addr)
        .context("invalid iroh bind address")?
        .alpns(vec![IROH_ALPN.to_vec()])
        .bind()
        .await
        .context("bind iroh endpoint")?;
    let listen = endpoint.clone();
    tokio::spawn(async move {
        while let Some(incoming) = listen.accept().await {
            let state = state.clone();
            let allowed_peers = allowed_peers.clone();
            tokio::spawn(async move {
                let connection = match incoming.await {
                    Ok(connection) => connection,
                    Err(error) => {
                        tracing::debug!(%error, "iroh webhook connection failed");
                        return;
                    }
                };
                let remote = connection.remote_id();
                if !allow_any && !allowed_peers.contains(&remote) {
                    tracing::warn!(%remote, "rejected unlisted iroh webhook peer");
                    connection.close(1u32.into(), b"peer not allowed");
                    return;
                }
                if let Err(error) = handle_iroh_connection(state, connection).await {
                    tracing::warn!(%error, %remote, "iroh webhook failed");
                }
            });
        }
    });
    Ok(endpoint)
}

async fn handle_iroh_connection(
    state: Arc<AppState>,
    connection: iroh::endpoint::Connection,
) -> Result<()> {
    let (mut send, mut recv) = connection.accept_bi().await.context("accept iroh stream")?;
    let bytes = recv
        .read_to_end(MAX_BODY + 1)
        .await
        .context("read iroh frame")?;
    ensure!(bytes.len() <= MAX_BODY, "iroh webhook exceeds body budget");
    let request: IrohRequest = serde_json::from_slice(&bytes).context("decode iroh webhook")?;
    let headers = HeaderMap::new();
    let body = serde_json::to_vec(&request.body).context("encode iroh webhook body")?;
    let response = state.ingest(&request.provider, &headers, &body, true);
    let encoded = serde_json::to_vec(&response).context("encode iroh response")?;
    send.write_all(&encoded)
        .await
        .context("write iroh response")?;
    send.finish().context("finish iroh response")?;
    // Keep the connection alive until the peer has consumed the response.
    // Dropping the connection immediately can abort the final stream write.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), connection.closed()).await;
    Ok(())
}

pub async fn push_iroh(
    endpoint_id: EndpointId,
    address: SocketAddr,
    provider: String,
    body: Value,
) -> Result<IngestResponse> {
    ensure!(body.is_object(), "webhook body must be a JSON object");
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .context("invalid local iroh address")?
        .bind()
        .await
        .context("bind iroh client")?;
    let addr = EndpointAddr::new(endpoint_id).with_ip_addr(address);
    let connection = endpoint
        .connect(addr, IROH_ALPN)
        .await
        .context("connect iroh webhook receiver")?;
    let (mut send, mut recv) = connection.open_bi().await.context("open iroh stream")?;
    let request = IrohRequest {
        provider,
        headers: BTreeMap::new(),
        body,
    };
    let encoded = serde_json::to_vec(&request).context("encode iroh webhook")?;
    ensure!(encoded.len() <= MAX_BODY, "webhook exceeds body budget");
    send.write_all(&encoded)
        .await
        .context("write iroh webhook")?;
    send.finish().context("finish iroh webhook")?;
    let response = recv
        .read_to_end(MAX_BODY)
        .await
        .context("read iroh response")?;
    endpoint.close().await;
    serde_json::from_slice(&response).context("decode iroh response")
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn delivery_id(headers: &HeaderMap, body: &Value) -> String {
    header(headers, "webhook-id")
        .or_else(|| header(headers, "x-github-delivery"))
        .or_else(|| header(headers, "x-request-id"))
        .map(str::to_string)
        .unwrap_or_else(|| {
            let digest = Sha256::digest(serde_json::to_vec(body).unwrap_or_default());
            format!("sha256:{}", hex::encode(digest))
        })
}

fn safe_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut safe = BTreeMap::new();
    for (name, value) in headers {
        let name = name.as_str();
        if matches!(
            name,
            "authorization"
                | "cookie"
                | "webhook-signature"
                | "x-api-key"
                | "x-confluence-token"
                | "x-fpl-ingress-secret"
        ) {
            continue;
        }
        if (matches!(name, "content-type" | "user-agent") || name.starts_with("x-"))
            && let Ok(value) = value.to_str()
        {
            safe.insert(name.to_string(), value.to_string());
        }
    }
    safe
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockPublisher {
        events: Mutex<Vec<ConfluenceEvent>>,
    }

    impl Publisher for MockPublisher {
        fn publish(&self, event: &ConfluenceEvent) -> Result<String> {
            self.events.lock().unwrap().push(event.clone());
            Ok(format!("request-{}", self.events.lock().unwrap().len()))
        }
    }

    fn signed_headers(secret: &[u8], body: &[u8]) -> HeaderMap {
        let webhook = standardwebhooks::Webhook::from_bytes(secret.to_vec()).unwrap();
        let now = now_ms() / 1000;
        let signature = webhook.sign("delivery-1", now as i64, body).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("webhook-id", "delivery-1".parse().unwrap());
        headers.insert("webhook-timestamp", now.to_string().parse().unwrap());
        headers.insert("webhook-signature", signature.parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        headers
    }

    #[test]
    fn publishes_signed_webhook_and_deduplicates() {
        let secret = vec![42u8; 32];
        let publisher = Arc::new(MockPublisher::default());
        let state = AppState::new(
            ConfluenceConfig {
                tenant: "default".into(),
                webhook_secret: Some(secret.clone()),
                ingress_token: None,
                allow_unsigned: false,
            },
            publisher.clone(),
        );
        let body = br#"{"action":"opened"}"#;
        let headers = signed_headers(&secret, body);
        let first = state.ingest("github", &headers, body, false);
        assert_eq!(first.status, 202);
        assert!(!first.duplicate);
        let second = state.ingest("github", &headers, body, false);
        assert_eq!(second.status, 200);
        assert!(second.duplicate);
        let events = publisher.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider, "github");
        assert_eq!(events[0].delivery_id, "delivery-1");
        assert_eq!(events[0].tenant, "default");
    }

    #[test]
    fn rejects_bad_signature_and_unsigned_requests() {
        let secret = vec![42u8; 32];
        let publisher = Arc::new(MockPublisher::default());
        let state = AppState::new(
            ConfluenceConfig {
                tenant: "default".into(),
                webhook_secret: Some(secret.clone()),
                ingress_token: None,
                allow_unsigned: false,
            },
            publisher,
        );
        let body = br#"{"action":"opened"}"#;
        let mut headers = signed_headers(&secret, body);
        headers.insert("webhook-signature", "v1,bad".parse().unwrap());
        assert_eq!(state.ingest("github", &headers, body, false).status, 401);
        assert_eq!(
            state
                .ingest("github", &HeaderMap::new(), body, false)
                .status,
            401
        );
    }

    #[tokio::test]
    async fn iroh_round_trip_publishes_one_event() {
        let publisher = Arc::new(MockPublisher::default());
        let state = AppState::new(
            ConfluenceConfig {
                tenant: "default".into(),
                webhook_secret: None,
                ingress_token: None,
                allow_unsigned: true,
            },
            publisher.clone(),
        );
        let endpoint = bind_iroh(
            Arc::new(state),
            SecretKey::generate(),
            "127.0.0.1:0".parse().unwrap(),
            Vec::new(),
            true,
        )
        .await
        .unwrap();
        let address = endpoint
            .bound_sockets()
            .into_iter()
            .find(|address| address.ip().is_loopback())
            .expect("loopback iroh address");
        let response = push_iroh(
            endpoint.id(),
            address,
            "iroh-test".into(),
            serde_json::json!({"via":"iroh"}),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 202);
        {
            let events = publisher.events.lock().unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].provider, "iroh-test");
        }
        endpoint.close().await;
    }

    #[test]
    fn rejects_large_or_non_object_bodies() {
        let publisher = Arc::new(MockPublisher::default());
        let state = AppState::new(
            ConfluenceConfig {
                tenant: "default".into(),
                webhook_secret: None,
                ingress_token: None,
                allow_unsigned: true,
            },
            publisher,
        );
        assert_eq!(
            state
                .ingest("test", &HeaderMap::new(), &vec![b' '; MAX_BODY + 1], false)
                .status,
            413
        );
        assert_eq!(
            state.ingest("test", &HeaderMap::new(), b"[]", false).status,
            415
        );
    }
}
