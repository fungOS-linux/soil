use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use http::Method;
use serde_json::Value;
use soil_wasi_host::{
    Algorithm, CredentialProvider, GatewayRoute, HttpGatewayBuilder, HttpService, MemoryBus,
    StaticCredentials, WasiHostBuilder, WasiHostPolicy,
};
use tokio::{net::TcpListener, task::JoinHandle};

mod support;
use support::{post, response_body, send};

const CONFLUENCE: &[u8] = include_bytes!("../fixtures/confluence.wasm");
const SECRET: &[u8] = b"a confluence webhook secret";
const CREDENTIAL: &str = "confluence.webhook";

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn signature(credentials: &StaticCredentials, id: &str, timestamp: i64, body: &str) -> String {
    let signable = format!("{id}.{timestamp}.{body}");
    let handle = credentials.resolve(CREDENTIAL).unwrap();
    let signature = credentials
        .sign(&handle.id, Algorithm::HmacSha256, signable.as_bytes())
        .unwrap();
    format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(signature)
    )
}

async fn start(
    sink: Arc<MemoryBus>,
) -> (
    std::net::SocketAddr,
    JoinHandle<anyhow::Result<()>>,
    Arc<StaticCredentials>,
) {
    let mut credentials = StaticCredentials::new();
    credentials
        .insert_hmac(CREDENTIAL, SECRET.to_vec(), "verify confluence webhooks")
        .unwrap();
    let credentials = Arc::new(credentials);
    let builder = WasiHostBuilder::new(WasiHostPolicy::default())
        .credential_provider(credentials.clone())
        .bus(sink);
    let service = Arc::new(
        HttpService::from_bytes_named("dev.fungos.confluence", CONFLUENCE, builder).unwrap(),
    );
    let gateway = HttpGatewayBuilder::new()
        .max_body_bytes(64 * 1024)
        .vhost(
            "confluence.test",
            "dev.fungos.confluence",
            service,
            [GatewayRoute::new([Method::POST], "/webhooks/").unwrap()],
        )
        .unwrap()
        .build()
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(Arc::new(gateway).serve(listener));
    (address, task, credentials)
}

#[tokio::test]
async fn verifies_signs_and_publishes_one_event() {
    let sink = Arc::new(MemoryBus::default());
    let (address, task, credentials) = start(sink.clone()).await;

    let body = r#"{"action":"opened","page":{"id":"123"}}"#;
    let timestamp = now_seconds();
    let timestamp_text = timestamp.to_string();
    let signature = signature(&credentials, "delivery-1", timestamp, body);
    let headers = [
        ("content-type", "application/json"),
        ("webhook-id", "delivery-1"),
        ("webhook-timestamp", timestamp_text.as_str()),
        ("webhook-signature", signature.as_str()),
        ("x-event-type", "page_updated"),
    ];

    let response = send(
        address,
        &post("confluence.test", "/webhooks/provider-a", body, &headers),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 202"), "{response}");
    let reply: Value = serde_json::from_str(&response_body(&response)).unwrap();
    assert_eq!(reply["status"], 202);
    assert_eq!(reply["duplicate"], false);
    assert_eq!(reply["request_id"], "local-1");

    let frames = sink.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].kind, "confluence.webhook.received.v1");
    let event: Value = serde_json::from_slice(&frames[0].body).unwrap();
    assert_eq!(event["provider"], "provider-a");
    assert_eq!(event["delivery_id"], "delivery-1");
    assert_eq!(event["event_type"], "page_updated");
    assert_eq!(event["body"]["action"], "opened");
    assert!(
        event.get("tenant").is_none(),
        "tenant is host-stamped: {event}"
    );

    // `soil:unibus` has no duplicate receipt: with a raw `MemoryBus` (no
    // `ConfluenceBus` dedupe), a redelivery is a second accepted frame and
    // still a 202. Delivery dedupe lives in `ConfluenceBus` and is proven in
    // `soil-confluence`.
    let duplicate = send(
        address,
        &post("confluence.test", "/webhooks/provider-a", body, &headers),
    )
    .await;
    assert!(duplicate.starts_with("HTTP/1.1 202"), "{duplicate}");
    let reply: Value = serde_json::from_str(&response_body(&duplicate)).unwrap();
    assert_eq!(reply["request_id"], "local-2");
    assert_eq!(sink.frames().len(), 2);

    task.abort();
}

#[tokio::test]
async fn rejects_bad_signatures_stale_timestamps_and_wrong_methods() {
    let sink = Arc::new(MemoryBus::default());
    let (address, task, credentials) = start(sink.clone()).await;

    let body = r#"{"action":"opened"}"#;
    let timestamp = now_seconds();
    let timestamp_text = timestamp.to_string();

    let tampered = [
        ("content-type", "application/json"),
        ("webhook-id", "delivery-2"),
        ("webhook-timestamp", timestamp_text.as_str()),
        ("webhook-signature", "v1,AAAA"),
    ];
    let response = send(
        address,
        &post("confluence.test", "/webhooks/provider-a", body, &tampered),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");

    let stale = timestamp - 1000;
    let stale_text = stale.to_string();
    let stale_signature = signature(&credentials, "delivery-3", stale, body);
    let stale_headers = [
        ("content-type", "application/json"),
        ("webhook-id", "delivery-3"),
        ("webhook-timestamp", stale_text.as_str()),
        ("webhook-signature", stale_signature.as_str()),
    ];
    let response = send(
        address,
        &post(
            "confluence.test",
            "/webhooks/provider-a",
            body,
            &stale_headers,
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");

    let response = send(
        address,
        "GET /webhooks/provider-a HTTP/1.1\r\nHost: confluence.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 405"), "{response}");

    let response = send(
        address,
        &post("confluence.test", "/webhooks/", body, &stale_headers),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");

    assert!(sink.frames().is_empty());
    task.abort();
}

#[tokio::test]
async fn rejects_oversized_and_non_object_bodies() {
    let sink = Arc::new(MemoryBus::default());
    let (address, task, credentials) = start(sink.clone()).await;

    // The gateway rejects on Content-Length before reading; do not write the
    // body, so the client is not racing a close for 70 KiB it already sent.
    let response = send(
        address,
        "POST /webhooks/provider-a HTTP/1.1\r\nHost: confluence.test\r\nContent-Length: 71680\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");

    let body = "[1,2,3]";
    let timestamp = now_seconds();
    let timestamp_text = timestamp.to_string();
    let signature = signature(&credentials, "delivery-4", timestamp, body);
    let headers = [
        ("content-type", "application/json"),
        ("webhook-id", "delivery-4"),
        ("webhook-timestamp", timestamp_text.as_str()),
        ("webhook-signature", signature.as_str()),
    ];
    let response = send(
        address,
        &post("confluence.test", "/webhooks/provider-a", body, &headers),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 415"), "{response}");

    assert!(sink.frames().is_empty());
    task.abort();
}
