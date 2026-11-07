//! Host-owned HTTP ingress for Spores.
//!
//! One listener can serve several Spores. The host decides the vhost, the
//! method and path grants, and the body ceiling; the guest never sees a route
//! it was not granted. Every invocation carries a [`HttpContext`] that names
//! the granted Spore and route rather than trusting request headers.

use std::{collections::BTreeSet, convert::Infallible, fmt, pin::Pin, sync::Arc, time::Duration};

use anyhow::{Result, bail};
use bytes::Bytes;
use http::{Method, StatusCode};
use http_body::{Body, Frame, SizeHint};
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use tokio::net::TcpListener;
use wasmtime_wasi_http::{Error as WasiHttpError, io::TokioIo, p2::body::HyperOutgoingBody};

use crate::{HttpContext, service::HttpService};

/// A host grant for one path prefix on a vhost.
#[derive(Clone, Debug)]
pub struct GatewayRoute {
    methods: BTreeSet<Method>,
    prefix: String,
    max_body_bytes: Option<usize>,
}

impl GatewayRoute {
    /// Grant a method set on a path prefix.
    ///
    /// A prefix matches `prefix` itself and any path below it. `/webhooks/`
    /// matches `/webhooks/github`; `/webhooks` does not match `/webhooksx`.
    pub fn new(
        methods: impl IntoIterator<Item = Method>,
        prefix: impl Into<String>,
    ) -> Result<Self> {
        let methods: BTreeSet<Method> = methods.into_iter().collect();
        let prefix = prefix.into();
        if methods.is_empty() {
            bail!("route {prefix:?} grants no methods");
        }
        validate_prefix(&prefix)?;
        Ok(Self {
            methods,
            prefix,
            max_body_bytes: None,
        })
    }

    /// Override the gateway body ceiling for this route.
    pub fn max_body_bytes(mut self, limit: usize) -> Self {
        self.max_body_bytes = Some(limit);
        self
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn methods(&self) -> impl Iterator<Item = &Method> {
        self.methods.iter()
    }

    fn allows(&self, method: &Method) -> bool {
        self.methods.contains(method)
    }

    fn matches(&self, path: &str) -> bool {
        if path == self.prefix {
            return true;
        }
        let Some(rest) = path.strip_prefix(&self.prefix) else {
            return false;
        };
        self.prefix.ends_with('/') || rest.starts_with('/')
    }
}

/// A compiled Spore plus the hostnames and routes it may serve.
#[derive(Clone)]
pub struct GatewayVhost {
    host: String,
    spore: String,
    service: Arc<HttpService>,
    routes: Vec<GatewayRoute>,
}

impl GatewayVhost {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn spore(&self) -> &str {
        &self.spore
    }
}

#[derive(Clone)]
pub struct HttpGatewayBuilder {
    vhosts: Vec<GatewayVhost>,
    max_body_bytes: usize,
    request_timeout: Duration,
}

impl Default for HttpGatewayBuilder {
    fn default() -> Self {
        Self {
            vhosts: Vec::new(),
            max_body_bytes: 1024 * 1024,
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl HttpGatewayBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant a hostname to a Spore.
    ///
    /// The hostname is matched exactly after lowercasing and stripping a port
    /// and a trailing dot. Hostnames and Spore names may appear once each.
    pub fn vhost(
        mut self,
        host: impl Into<String>,
        spore: impl Into<String>,
        service: Arc<HttpService>,
        routes: impl IntoIterator<Item = GatewayRoute>,
    ) -> Result<Self> {
        let host = normalize_host(&host.into())?;
        let spore = spore.into();
        if spore.trim().is_empty() {
            bail!("vhost {host}: Spore name must not be empty");
        }
        if self.vhosts.iter().any(|existing| existing.host == host) {
            bail!("vhost {host} is already granted");
        }
        let routes: Vec<GatewayRoute> = routes.into_iter().collect();
        if routes.is_empty() {
            bail!("vhost {host} grants no routes");
        }
        self.vhosts.push(GatewayVhost {
            host,
            spore,
            service,
            routes,
        });
        Ok(self)
    }

    /// Default request body ceiling, in bytes.
    pub fn max_body_bytes(mut self, limit: usize) -> Self {
        self.max_body_bytes = limit;
        self
    }

    /// Deadline for the guest to produce a response.
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn build(self) -> std::result::Result<HttpGateway, IngressError> {
        if self.vhosts.is_empty() {
            return Err(IngressError::Empty);
        }
        if self.max_body_bytes == 0 {
            return Err(IngressError::ZeroBodyLimit);
        }
        Ok(HttpGateway {
            vhosts: self.vhosts,
            max_body_bytes: self.max_body_bytes,
            request_timeout: self.request_timeout,
        })
    }
}

/// The built ingress listener policy.
#[derive(Clone)]
pub struct HttpGateway {
    vhosts: Vec<GatewayVhost>,
    max_body_bytes: usize,
    request_timeout: Duration,
}

impl HttpGateway {
    pub fn builder() -> HttpGatewayBuilder {
        HttpGatewayBuilder::new()
    }

    pub fn vhosts(&self) -> &[GatewayVhost] {
        &self.vhosts
    }

    /// Serve HTTP/1.1 until the listener fails or the task is cancelled.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        loop {
            let (client, remote) = listener.accept().await?;
            let gateway = Arc::clone(&self);
            tokio::spawn(async move {
                let result = http1::Builder::new()
                    .keep_alive(true)
                    .serve_connection(
                        TokioIo::new(client),
                        service_fn(move |request| {
                            let gateway = Arc::clone(&gateway);
                            async move {
                                Ok::<_, Infallible>(gateway.handle(request, remote).await)
                            }
                        }),
                    )
                    .await;
                if let Err(error) = result {
                    eprintln!("soil-http-gateway: {remote}: {error}");
                }
            });
        }
    }

    async fn handle(
        &self,
        request: Request<Incoming>,
        remote: std::net::SocketAddr,
    ) -> Response<HyperOutgoingBody> {
        let Some(authority) = request_authority(&request) else {
            return text_response(StatusCode::BAD_REQUEST, "missing or invalid Host");
        };
        let Some(vhost) = self.vhosts.iter().find(|vhost| vhost.host == authority) else {
            // An unknown vhost is not a Spore error and must not reveal the
            // grant table. The caller cannot distinguish it from a typo.
            return text_response(StatusCode::NOT_FOUND, "unknown host");
        };

        let path = request.uri().path();
        let Some(route) = vhost.routes.iter().find(|route| route.matches(path)) else {
            return text_response(StatusCode::NOT_FOUND, "no route grant");
        };
        if !route.allows(request.method()) {
            let allow = allow_header(route);
            let mut response = text_response(StatusCode::METHOD_NOT_ALLOWED, "method not granted");
            if let Ok(value) = allow.parse() {
                response.headers_mut().insert(http::header::ALLOW, value);
            }
            return response;
        }

        let limit = route.max_body_bytes.unwrap_or(self.max_body_bytes);
        if request
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|length| length > limit as u64)
        {
            return text_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
        }

        let context = HttpContext::new(
            vhost.spore.clone(),
            route.prefix.clone(),
            Some(remote.to_string()),
            None,
        );
        let service = Arc::clone(&vhost.service);
        let request = request.map(|body| CappedBody::new(body, limit));
        match tokio::time::timeout(self.request_timeout, service.handle(request, context)).await {
            Ok(response) => response,
            Err(_) => text_response(StatusCode::GATEWAY_TIMEOUT, "guest exceeded its deadline"),
        }
    }
}

/// Why a gateway could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngressError {
    Empty,
    ZeroBodyLimit,
    InvalidHost(String),
}

impl fmt::Display for IngressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("HTTP gateway has no vhost grants"),
            Self::ZeroBodyLimit => f.write_str("HTTP gateway body limit must be positive"),
            Self::InvalidHost(host) => write!(f, "invalid vhost {host:?}"),
        }
    }
}

impl std::error::Error for IngressError {}

/// A request body that fails closed at a byte ceiling.
///
/// The guest sees a body-stream error rather than a truncated body, so a Spore
/// that reads `IncomingBody` cannot mistake an over-limit request for a short
/// one. `Content-Length` is rejected by the gateway before the guest starts.
struct CappedBody<B> {
    inner: Pin<Box<B>>,
    remaining: usize,
    limit: usize,
}

impl<B> CappedBody<B> {
    fn new(inner: B, limit: usize) -> Self {
        Self {
            inner: Box::pin(inner),
            remaining: limit,
            limit,
        }
    }
}

impl<B, E> Body for CappedBody<B>
where
    B: Body<Data = Bytes, Error = E>,
    E: Into<WasiHttpError>,
{
    type Data = Bytes;
    type Error = WasiHttpError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::result::Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_frame(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    if data.len() > this.remaining {
                        this.remaining = 0;
                        return std::task::Poll::Ready(Some(Err(
                            WasiHttpError::HttpRequestBodySize(Some(this.limit as u64)),
                        )));
                    }
                    this.remaining -= data.len();
                }
                std::task::Poll::Ready(Some(Ok(frame)))
            }
            std::task::Poll::Ready(Some(Err(error))) => {
                std::task::Poll::Ready(Some(Err(error.into())))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

fn request_authority(request: &Request<Incoming>) -> Option<String> {
    let raw = request
        .headers()
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or_else(|| request.uri().authority().map(|value| value.to_string()))?;
    let authority = raw.parse::<http::uri::Authority>().ok()?;
    normalize_host(authority.host()).ok()
}

fn normalize_host(host: &str) -> std::result::Result<String, IngressError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host.contains('/')
        || host.contains('@')
        || host.chars().any(char::is_whitespace)
        || host == "*"
    {
        return Err(IngressError::InvalidHost(host));
    }
    Ok(host)
}

fn validate_prefix(prefix: &str) -> Result<()> {
    if !prefix.starts_with('/') {
        bail!("route {prefix:?} must start with `/`");
    }
    if prefix.contains('?') || prefix.contains('#') {
        bail!("route {prefix:?} must not contain a query or fragment");
    }
    if prefix.ends_with("//") {
        bail!("route {prefix:?} must not end with `//`");
    }
    Ok(())
}

fn allow_header(route: &GatewayRoute) -> String {
    route
        .methods()
        .map(Method::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn text_response(status: StatusCode, message: &str) -> Response<HyperOutgoingBody> {
    let body = Full::new(Bytes::from(message.to_string()))
        .map_err(|never| match never {})
        .boxed_unsync();
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .expect("static response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_matches_method_and_prefix() {
        let route = GatewayRoute::new([Method::POST], "/webhooks/").unwrap();
        assert!(route.allows(&Method::POST));
        assert!(!route.allows(&Method::GET));
        assert!(route.matches("/webhooks/github"));
        assert!(route.matches("/webhooks/"));
        assert!(!route.matches("/webhooks"));
        assert!(!route.matches("/webhook"));

        let bare = GatewayRoute::new([Method::GET], "/health").unwrap();
        assert!(bare.matches("/health"));
        assert!(bare.matches("/health/ready"));
        assert!(!bare.matches("/healthz"));
    }

    #[test]
    fn host_normalization_rejects_ambiguous_names() {
        assert_eq!(
            normalize_host("Confluence.Test.").unwrap(),
            "confluence.test"
        );
        assert!(normalize_host("").is_err());
        assert!(normalize_host("*").is_err());
        assert!(normalize_host("a/b").is_err());
    }
}
