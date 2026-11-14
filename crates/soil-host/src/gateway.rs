//! Host-owned HTTP ingress for Spores.
//!
//! One listener can serve several Spores. The host decides the vhost, the
//! method and path grants, and the body ceiling; the guest never sees a route
//! it was not granted. Every invocation carries a [`HttpContext`] that names
//! the granted Spore and route rather than trusting request headers.
//!
//! The listener speaks plain HTTP, so the only identity it can report is the
//! peer address — unless an [`OriginAuth::TrustedTerminator`] sits in front and
//! verified a client certificate. That assertion reaches the guest through
//! [`HttpContext::tls_peer`], never through a request header a Spore could be
//! tricked into reading.

use std::{
    collections::BTreeSet, convert::Infallible, fmt, net::IpAddr, pin::Pin, sync::Arc,
    time::Duration,
};

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

/// How the host decides whether a request arrived through an authenticated
/// transport.
///
/// This is transport truth only: it says what the listener can prove about the
/// connection, not whether the request should be acted on. A provider webhook
/// signature is the Spore's job; this decides what the host may assert to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OriginAuth {
    /// Nothing is asserted about the transport; `tls_peer` stays empty and the
    /// Spore verifies the caller itself (a webhook HMAC, for example). Any peer
    /// may connect.
    #[default]
    ProviderSignature,
    /// A TLS terminator in front of this listener verified the caller — a
    /// client certificate, Cloudflare Authenticated Origin Pulls, an Access
    /// token — and reported it in the trusted-terminator headers. Only the
    /// listed peers may connect at all, because a direct hit from anywhere else
    /// is precisely the bypass this policy exists to refuse.
    TrustedTerminator { peers: Vec<IpAddr> },
}

impl OriginAuth {
    /// Where the terminator names the verifier that accepted the caller. Its
    /// value is what the gateway logs as `verifier`.
    pub const ORIGIN_HEADER: &str = "x-palisade-origin";

    /// Where the terminator reports the subject certificate it verified, e.g.
    /// a certificate fingerprint. Its value becomes [`HttpContext::tls_peer`].
    pub const AUTH_HEADER: &str = "x-palisade-auth";

    /// Trust assertions from `peers` only.
    pub fn trusted_terminator(peers: impl IntoIterator<Item = IpAddr>) -> Self {
        Self::TrustedTerminator {
            peers: peers.into_iter().collect(),
        }
    }

    /// Decide what this request may assert, or `None` if the peer may not
    /// connect under this policy.
    ///
    /// The assertion headers are removed from the request in every case. A
    /// Spore cannot tell a forged header from an asserted one, so the host
    /// never lets either through: the value travels in [`HttpContext`].
    fn verify<B>(&self, request: &mut Request<B>, peer: IpAddr) -> Option<Assertion> {
        let headers = request.headers_mut();
        let origin = headers.remove(Self::ORIGIN_HEADER);
        let subject = headers.remove(Self::AUTH_HEADER);

        match self {
            Self::ProviderSignature => Some(Assertion {
                verifier: "provider-signature".to_owned(),
                tls_peer: None,
            }),
            Self::TrustedTerminator { peers } => {
                if !peers.contains(&peer) {
                    return None;
                }
                // A trusted peer that asserts nothing simply did not verify a
                // caller on this request. That is not a failure: the Spore
                // still checks its own signature. Reporting the absence keeps
                // the log honest rather than implying a verifier ran.
                Some(Assertion {
                    verifier: header_text(origin).unwrap_or_else(|| "none".to_owned()),
                    tls_peer: header_text(subject),
                })
            }
        }
    }
}

/// What an [`OriginAuth`] policy proved about one request.
struct Assertion {
    /// Name of the verifier that accepted the caller, for the audit log.
    verifier: String,
    /// The verified subject, when the terminator saw one.
    tls_peer: Option<String>,
}

fn header_text(value: Option<http::HeaderValue>) -> Option<String> {
    let text = value?.to_str().ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
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
    origin_auth: OriginAuth,
    max_body_bytes: usize,
    request_timeout: Duration,
}

impl Default for HttpGatewayBuilder {
    fn default() -> Self {
        Self {
            vhosts: Vec::new(),
            origin_auth: OriginAuth::default(),
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

    /// What the listener may assert about the caller's transport.
    ///
    /// Applies to every vhost on the listener: which address a terminator
    /// connects from does not depend on the hostname, and a second guarantee
    /// for a second hostname is a second listener.
    pub fn origin_auth(mut self, origin_auth: OriginAuth) -> Self {
        self.origin_auth = origin_auth;
        self
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
        validate_origin_auth(&self.origin_auth)?;
        Ok(HttpGateway {
            vhosts: self.vhosts,
            origin_auth: self.origin_auth,
            max_body_bytes: self.max_body_bytes,
            request_timeout: self.request_timeout,
        })
    }
}

/// The built ingress listener policy.
#[derive(Clone)]
pub struct HttpGateway {
    vhosts: Vec<GatewayVhost>,
    origin_auth: OriginAuth,
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
        mut request: Request<Incoming>,
        remote: std::net::SocketAddr,
    ) -> Response<HyperOutgoingBody> {
        let Some(assertion) = self.origin_auth.verify(&mut request, remote.ip()) else {
            // Refused before the grant table is consulted: a direct hit on the
            // origin is not a routing miss, and answering 404 would invite the
            // caller to keep guessing hostnames.
            tracing::warn!(peer = %remote, "refused: not a trusted terminator");
            return text_response(StatusCode::FORBIDDEN, "untrusted origin");
        };

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

        tracing::info!(
            vhost = %authority,
            spore = %vhost.spore,
            path,
            verifier = %assertion.verifier,
            tls_peer = assertion.tls_peer.as_deref().unwrap_or(""),
            "accepted request"
        );

        let context = HttpContext::new(
            vhost.spore.clone(),
            route.prefix.clone(),
            Some(remote.to_string()),
            assertion.tls_peer,
        );
        let service = Arc::clone(&vhost.service);
        let answered_path = path.to_owned();
        let request = request.map(|body| CappedBody::new(body, limit));
        // The status is the one fact an operator needs and the Spore cannot be
        // trusted to log: a 401 and a 202 look identical in an access line that
        // stops at "accepted".
        let response = match tokio::time::timeout(
            self.request_timeout,
            service.handle(request, context),
        )
        .await
        {
            Ok(response) => response,
            Err(_) => text_response(StatusCode::GATEWAY_TIMEOUT, "guest exceeded its deadline"),
        };
        tracing::info!(
            spore = %vhost.spore,
            path = %answered_path,
            status = response.status().as_u16(),
            "spore answered"
        );
        response
    }
}

fn validate_origin_auth(origin_auth: &OriginAuth) -> std::result::Result<(), IngressError> {
    let OriginAuth::TrustedTerminator { peers } = origin_auth else {
        return Ok(());
    };
    if peers.is_empty() {
        return Err(IngressError::NoTrustedTerminator);
    }
    match peers.iter().find(|peer| peer.is_unspecified()) {
        Some(peer) => Err(IngressError::UnspecifiedTrustedTerminator(*peer)),
        None => Ok(()),
    }
}

/// Why a gateway could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngressError {
    Empty,
    ZeroBodyLimit,
    InvalidHost(String),
    /// A trusted-terminator policy named no peers, so it could never accept a
    /// request; that is a typo, not a lockout.
    NoTrustedTerminator,
    /// `0.0.0.0` is not a peer address.
    UnspecifiedTrustedTerminator(IpAddr),
}

impl fmt::Display for IngressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("HTTP gateway has no vhost grants"),
            Self::ZeroBodyLimit => f.write_str("HTTP gateway body limit must be positive"),
            Self::InvalidHost(host) => write!(f, "invalid vhost {host:?}"),
            Self::NoTrustedTerminator => {
                f.write_str("trusted-terminator origin auth names no peers")
            }
            Self::UnspecifiedTrustedTerminator(peer) => {
                write!(f, "trusted-terminator peer {peer} is not a host address")
            }
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

    fn asserted(auth: &OriginAuth, peer: [u8; 4]) -> Option<Assertion> {
        let mut request = Request::builder()
            .uri("http://confluence.test/webhooks/github")
            .body(())
            .unwrap();
        request.headers_mut().extend(request_headers());
        let assertion = auth.verify(&mut request, IpAddr::from(peer))?;
        // The assertion headers never reach the guest, whatever the policy.
        assert!(request.headers().get(OriginAuth::ORIGIN_HEADER).is_none());
        assert!(request.headers().get(OriginAuth::AUTH_HEADER).is_none());
        assert!(
            request.headers().get("x-palisade-unrelated").is_some(),
            "only the assertion headers are stripped"
        );
        Some(assertion)
    }

    fn request_headers() -> Vec<(http::HeaderName, http::HeaderValue)> {
        [
            (OriginAuth::ORIGIN_HEADER, "cf-aop"),
            (OriginAuth::AUTH_HEADER, "sha256:5f1e"),
            ("x-palisade-unrelated", "kept"),
        ]
        .into_iter()
        .map(|(name, value)| (name.parse().unwrap(), value.parse().unwrap()))
        .collect()
    }

    #[test]
    fn provider_signature_ignores_the_transport() {
        let assertion = asserted(&OriginAuth::ProviderSignature, [10, 0, 0, 1])
            .expect("every peer may connect");
        assert_eq!(assertion.verifier, "provider-signature");
        assert_eq!(assertion.tls_peer, None);
    }

    #[test]
    fn trusted_terminator_refuses_other_peers() {
        let auth = OriginAuth::trusted_terminator([IpAddr::from([10, 0, 0, 2])]);
        assert!(
            asserted(&auth, [10, 0, 0, 1]).is_none(),
            "a direct hit on the origin is the bypass the policy exists to stop"
        );
    }

    #[test]
    fn trusted_terminator_reports_the_verified_certificate() {
        let auth = OriginAuth::trusted_terminator([IpAddr::from([127, 0, 0, 1])]);
        let assertion = asserted(&auth, [127, 0, 0, 1]).unwrap();
        assert_eq!(assertion.verifier, "cf-aop");
        assert_eq!(assertion.tls_peer.as_deref(), Some("sha256:5f1e"));
    }

    #[test]
    fn trusted_terminator_without_an_assertion_says_so() {
        let auth = OriginAuth::trusted_terminator([IpAddr::from([127, 0, 0, 1])]);
        let mut request = Request::builder()
            .uri("http://confluence.test/webhooks/github")
            .body(())
            .unwrap();
        let assertion = auth
            .verify(&mut request, IpAddr::from([127, 0, 0, 1]))
            .unwrap();
        assert_eq!(assertion.verifier, "none");
        assert_eq!(assertion.tls_peer, None);
    }

    #[test]
    fn trusted_terminator_rejects_an_unreadable_assertion() {
        let auth = OriginAuth::trusted_terminator([IpAddr::from([127, 0, 0, 1])]);
        let mut request = Request::builder()
            .uri("http://confluence.test/webhooks/github")
            .body(())
            .unwrap();
        request.headers_mut().insert(
            OriginAuth::ORIGIN_HEADER,
            http::HeaderValue::from_bytes(b"\xff\xfe").unwrap(),
        );
        let assertion = auth
            .verify(&mut request, IpAddr::from([127, 0, 0, 1]))
            .unwrap();
        assert_eq!(assertion.verifier, "none");
    }

    #[test]
    fn a_terminator_policy_needs_a_real_peer() {
        assert_eq!(
            validate_origin_auth(&OriginAuth::trusted_terminator([] as [IpAddr; 0])),
            Err(IngressError::NoTrustedTerminator)
        );
        assert_eq!(
            validate_origin_auth(&OriginAuth::trusted_terminator([IpAddr::from([
                0, 0, 0, 0
            ])])),
            Err(IngressError::UnspecifiedTrustedTerminator(IpAddr::from([
                0, 0, 0, 0
            ])))
        );
        assert_eq!(validate_origin_auth(&OriginAuth::ProviderSignature), Ok(()));
        assert_eq!(
            validate_origin_auth(&OriginAuth::trusted_terminator([IpAddr::from([
                127, 0, 0, 1
            ])])),
            Ok(())
        );
    }
}
