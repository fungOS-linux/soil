//! Bounded host implementation of `soil:http`.
//!
//! The guest names an absolute URL and a host-resolved credential handle. The
//! host owns the origin allowlist, request headers, TLS, redirects, body cap,
//! deadline and credential material. No socket, DNS API, proxy setting or
//! secret crosses the component boundary.

use std::{collections::BTreeSet, io::Read, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use url::Url;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({ path: "wit/http", world: "runner" });

use self::soil::http::http;

// The generated interface, re-exported so a host that links `soil:http` from
// its own store (rather than running a `runner`-world component) implements the
// same trait this crate does instead of a second copy of it.
pub use soil::http::http as api;

const ALLOWED_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "content-type",
    "user-agent",
    "if-none-match",
    "if-modified-since",
    "range",
];

/// One request in host-internal form.
///
/// The WIT types are generated per consumer, so two crates that import
/// `soil:http` cannot pass them to each other. Everything between the guest
/// boundary and the transport speaks these plain types instead, which is what
/// lets another host (mush) reuse this policy rather than keep a second copy.
#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    pub method: Method,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub credential: Option<String>,
    pub max_bytes: u32,
    pub timeout_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub final_url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Origin, method, header, or credential was not granted.
    Denied,
    /// URL, header, credential handle, or budget is malformed.
    Invalid,
    /// The caller exceeded the host response/time budget.
    OverBudget,
    /// The request exceeded its deadline.
    Timeout,
    /// DNS, connect, or proxy failure.
    Unavailable,
    /// TLS verification or handshake failure.
    Tls,
    /// Upstream returned an error that is not a normal HTTP response.
    Http,
    /// The host transport failed.
    Io,
    /// This host has no HTTP backend.
    Unsupported,
}

/// Host-owned policy for one `soil:http` installation.
#[derive(Clone, Debug)]
pub struct HttpPolicy {
    origins: BTreeSet<String>,
    max_bytes: usize,
    timeout: Duration,
}

impl HttpPolicy {
    pub fn new(
        origins: impl IntoIterator<Item = String>,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Self> {
        let mut allowed = BTreeSet::new();
        for origin in origins {
            allowed.insert(normalize_origin(&origin)?);
        }
        if max_bytes == 0 {
            anyhow::bail!("max_bytes must be positive");
        }
        Ok(Self {
            origins: allowed,
            max_bytes,
            timeout,
        })
    }

    pub fn allow(&mut self, origin: &str) -> Result<()> {
        self.origins.insert(normalize_origin(origin)?);
        Ok(())
    }

    pub fn origins(&self) -> impl Iterator<Item = &str> {
        self.origins.iter().map(String::as_str)
    }
}

/// Host credential resolver. A Geode adapter implements this trait; the
/// Spore only ever supplies the opaque handle.
pub trait CredentialProvider: Send + Sync {
    /// Return the header name/value to inject for `handle`.
    fn resolve(&self, handle: &str) -> Result<Option<(String, String)>>;
}

#[derive(Default)]
pub struct NoCredentials;

impl CredentialProvider for NoCredentials {
    fn resolve(&self, _handle: &str) -> Result<Option<(String, String)>> {
        Ok(None)
    }
}

/// One bearer token, handed to a guest that asks for the `default` handle.
///
/// The token stays in the host process: a CLI can read it from the environment
/// so it never appears in a Spore's arguments, its output, or `ps`.
pub struct BearerCredential {
    value: String,
}

impl BearerCredential {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            value: format!("Bearer {}", token.into()),
        }
    }
}

impl CredentialProvider for BearerCredential {
    fn resolve(&self, handle: &str) -> Result<Option<(String, String)>> {
        Ok((handle == "default").then(|| ("authorization".to_string(), self.value.clone())))
    }
}

/// One host-internal transport request. The Spore never sees this shape.
#[derive(Clone, Debug)]
pub struct TransportRequest {
    pub url: Url,
    pub method: http::Method,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
    pub max_bytes: usize,
}

impl TransportRequest {
    /// The `soil:http` enum value (`get`, `post`, …).
    ///
    /// Not the HTTP verb: `ureq` wants `GET`, the WIT wants `get`, and a
    /// transport that speaks the value rather than this crate's enum should not
    /// have to name the generated type to read it.
    pub fn method_name(&self) -> &'static str {
        match self.method {
            http::Method::Get => "get",
            http::Method::Head => "head",
            http::Method::Post => "post",
            http::Method::Put => "put",
            http::Method::Patch => "patch",
            http::Method::Delete => "delete",
        }
    }

    /// Build a request from the names a wire transport uses.
    ///
    /// The generated `Method` enum is not part of this crate's public surface,
    /// so an out-of-tree transport (and its tests) needs a way in that does not
    /// depend on it.
    pub fn from_parts(
        url: &str,
        method: &str,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<Self> {
        let method = match method.to_ascii_lowercase().as_str() {
            "get" => http::Method::Get,
            "head" => http::Method::Head,
            "post" => http::Method::Post,
            "put" => http::Method::Put,
            "patch" => http::Method::Patch,
            "delete" => http::Method::Delete,
            other => anyhow::bail!("{other:?} is not a soil:http method"),
        };
        Ok(Self {
            url: Url::parse(url).with_context(|| format!("parse {url}"))?,
            method,
            headers,
            body,
            timeout,
            max_bytes,
        })
    }
}

#[derive(Clone, Debug)]
pub struct TransportResponse {
    pub status: u16,
    pub final_url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The transport cannot serve this request at all, so a composed transport
    /// may try another one. This is the only variant that means "not me"; every
    /// other one is an answer.
    Unsupported,
    Denied,
    OverBudget,
    Timeout,
    Unavailable,
    Tls,
    Http,
    Io,
}

/// A host-owned HTTP transport. Direct DNS/TLS is the first implementation.
///
/// An Iroh/Mycelium transport implements the same trait: resolve a peer or
/// service to an Iroh endpoint, open a stream, and speak HTTP over it. A router
/// transport can choose direct vs Iroh from the Mycelium directory or observed
/// health. The Spore never selects the route.
pub trait HttpTransport: Send + Sync {
    fn fetch(&self, request: &TransportRequest) -> Result<TransportResponse, TransportError>;
}

/// Prefer one transport, fall back to another when the first says it cannot
/// serve the request.
///
/// The distinction is deliberate: [`TransportError::Unsupported`] means "not
/// this transport", and everything else — including a timeout or a refusal — is
/// an answer that must reach the caller. A fallback on *failure* would silently
/// route around a provider that explicitly declined a URL.
pub struct PreferProvider {
    provider: Arc<dyn HttpTransport>,
    fallback: Arc<dyn HttpTransport>,
}

impl PreferProvider {
    pub fn new(provider: Arc<dyn HttpTransport>, fallback: Arc<dyn HttpTransport>) -> Self {
        Self { provider, fallback }
    }
}

impl HttpTransport for PreferProvider {
    fn fetch(&self, request: &TransportRequest) -> Result<TransportResponse, TransportError> {
        match self.provider.fetch(request) {
            Err(TransportError::Unsupported) => self.fallback.fetch(request),
            other => other,
        }
    }
}

#[derive(Default)]
pub struct DirectTransport;

impl HttpTransport for DirectTransport {
    fn fetch(&self, request: &TransportRequest) -> Result<TransportResponse, TransportError> {
        let agent = ureq::AgentBuilder::new()
            .timeout(request.timeout)
            .redirects(0)
            .build();
        let mut call = agent.request(method_name(request.method), request.url.as_str());
        for (name, value) in &request.headers {
            call = call.set(name, value);
        }
        let sent = match &request.body {
            Some(body) => call.send_bytes(body),
            None => call.call(),
        };
        let response = match sent {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                let text = error.to_string().to_ascii_lowercase();
                return Err(
                    if text.contains("tls")
                        || text.contains("certificate")
                        || text.contains("handshake")
                    {
                        TransportError::Tls
                    } else {
                        TransportError::Unavailable
                    },
                );
            }
        };
        let status = response.status();
        let final_url = request.url.to_string();
        let mut headers = Vec::new();
        for name in response.headers_names() {
            if let Some(value) = response.header(&name) {
                headers.push((name, value.to_string()));
            }
        }
        let mut body = Vec::new();
        // HEAD promises no body; every other method can carry one, including
        // the 204 from a successful Hue or Spotify write.
        if request.method != http::Method::Head {
            response
                .into_reader()
                .take(request.max_bytes as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|_| TransportError::Io)?;
            if body.len() > request.max_bytes {
                return Err(TransportError::OverBudget);
            }
        }
        Ok(TransportResponse {
            status,
            final_url,
            headers,
            body,
        })
    }
}

fn method_name(method: http::Method) -> &'static str {
    match method {
        http::Method::Get => "GET",
        http::Method::Head => "HEAD",
        http::Method::Post => "POST",
        http::Method::Put => "PUT",
        http::Method::Patch => "PATCH",
        http::Method::Delete => "DELETE",
    }
}

pub struct HttpHost {
    policy: HttpPolicy,
    credentials: Arc<dyn CredentialProvider>,
    transport: Arc<dyn HttpTransport>,
    table: ResourceTable,
    wasi: WasiCtx,
}

impl HttpHost {
    pub fn new(policy: HttpPolicy) -> Self {
        Self {
            policy,
            credentials: Arc::new(NoCredentials),
            transport: Arc::new(DirectTransport),
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build(),
        }
    }

    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Replace the host transport. Used by a direct/Iroh router or a test
    /// transport; the guest cannot select it.
    pub fn with_transport(mut self, transport: Arc<dyn HttpTransport>) -> Self {
        self.transport = transport;
        self
    }

    pub fn policy(&self) -> &HttpPolicy {
        &self.policy
    }
}

impl WasiView for HttpHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl HttpHost {
    /// Run one request through the policy and the transport.
    ///
    /// This is the whole `soil:http` contract in plain types; the guest adapter
    /// below only converts. `perform` is free-standing for the same reason: mush
    /// links its own `soil:http` instance and calls it rather than keeping a
    /// second answer to "is this request allowed".
    pub fn perform(&mut self, request: Request) -> Result<Response, Error> {
        perform(&self.policy, &*self.credentials, &*self.transport, request)
    }
}

impl http::Host for HttpHost {
    fn fetch(&mut self, request: http::Request) -> Result<http::Response, http::Error> {
        fetch(&self.policy, &*self.credentials, &*self.transport, request)
    }
}

/// The one implementation of `soil:http` policy, in host-internal types.
///
/// Every host that links the interface calls this: the `runner`-world host
/// above, `soil-wasi-host` through `fetch` below, and mush directly. A second
/// copy would be a second answer to "is this request allowed".
pub fn perform(
    policy: &HttpPolicy,
    credentials: &dyn CredentialProvider,
    transport: &dyn HttpTransport,
    request: Request,
) -> Result<Response, Error> {
    let url = Url::parse(&request.url).map_err(|_| Error::Invalid)?;
    let origin = origin_of(&url).ok_or(Error::Invalid)?;
    if !policy.origins.contains(&origin) {
        return Err(Error::Denied);
    }
    if request.max_bytes == 0
        || request.max_bytes as usize > policy.max_bytes
        || request.timeout_ms == 0
        || Duration::from_millis(request.timeout_ms as u64) > policy.timeout
    {
        return Err(Error::OverBudget);
    }
    for (name, value) in &request.headers {
        if !ALLOWED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(Error::Invalid);
        }
        if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
            return Err(Error::Invalid);
        }
    }
    // A body belongs to a write. Allowing one on GET/HEAD would make a read
    // look like an action and is not something any transport here supports
    // predictably.
    if matches!(request.method, Method::Get | Method::Head) && request.body.is_some() {
        return Err(Error::Invalid);
    }
    if request
        .body
        .as_ref()
        .is_some_and(|body| body.len() > policy.max_bytes)
    {
        return Err(Error::OverBudget);
    }
    let mut auth = None;
    if let Some(handle) = &request.credential {
        let header = credentials.resolve(handle).map_err(|_| Error::Denied)?;
        let header = header.ok_or(Error::Denied)?;
        if header.0.contains(['\r', '\n']) || header.1.contains(['\r', '\n']) {
            return Err(Error::Denied);
        }
        auth = Some(header);
    }

    let mut headers = request.headers.clone();
    if let Some(header) = auth {
        headers.push(header);
    }
    let transport_request = TransportRequest {
        url,
        method: method_to_wit(request.method),
        headers,
        body: request.body,
        timeout: Duration::from_millis(request.timeout_ms as u64),
        max_bytes: request.max_bytes as usize,
    };
    let response = transport
        .fetch(&transport_request)
        .map_err(|error| match error {
            TransportError::Denied => Error::Denied,
            TransportError::OverBudget => Error::OverBudget,
            TransportError::Timeout => Error::Timeout,
            TransportError::Unavailable => Error::Unavailable,
            TransportError::Tls => Error::Tls,
            TransportError::Http => Error::Http,
            TransportError::Io => Error::Io,
            TransportError::Unsupported => Error::Unsupported,
        })?;
    Ok(Response {
        status: response.status,
        final_url: response.final_url,
        headers: response.headers,
        body: response.body,
    })
}

/// `soil:http` as a guest sees it: convert, run the policy, convert back.
pub fn fetch(
    policy: &HttpPolicy,
    credentials: &dyn CredentialProvider,
    transport: &dyn HttpTransport,
    request: http::Request,
) -> Result<http::Response, http::Error> {
    let response = perform(
        policy,
        credentials,
        transport,
        Request {
            url: request.url,
            method: method_from_wit(request.method),
            headers: request
                .headers
                .into_iter()
                .map(|header| (header.name, header.value))
                .collect(),
            body: request.body,
            credential: request.credential,
            max_bytes: request.max_bytes,
            timeout_ms: request.timeout_ms,
        },
    )
    .map_err(error_to_wit)?;
    Ok(http::Response {
        status: response.status,
        final_url: response.final_url,
        headers: response
            .headers
            .into_iter()
            .map(|(name, value)| http::Header { name, value })
            .collect(),
        body: response.body,
    })
}

fn method_from_wit(method: http::Method) -> Method {
    match method {
        http::Method::Get => Method::Get,
        http::Method::Head => Method::Head,
        http::Method::Post => Method::Post,
        http::Method::Put => Method::Put,
        http::Method::Patch => Method::Patch,
        http::Method::Delete => Method::Delete,
    }
}

fn method_to_wit(method: Method) -> http::Method {
    match method {
        Method::Get => http::Method::Get,
        Method::Head => http::Method::Head,
        Method::Post => http::Method::Post,
        Method::Put => http::Method::Put,
        Method::Patch => http::Method::Patch,
        Method::Delete => http::Method::Delete,
    }
}

fn error_to_wit(error: Error) -> http::Error {
    match error {
        Error::Denied => http::Error::Denied,
        Error::Invalid => http::Error::Invalid,
        Error::OverBudget => http::Error::OverBudget,
        Error::Timeout => http::Error::Timeout,
        Error::Unavailable => http::Error::Unavailable,
        Error::Tls => http::Error::Tls,
        Error::Http => http::Error::Http,
        Error::Io => http::Error::Io,
        Error::Unsupported => http::Error::Unsupported,
    }
}

fn normalize_origin(origin: &str) -> Result<String> {
    let url = Url::parse(origin).with_context(|| format!("parse origin {origin}"))?;
    origin_of(&url).ok_or_else(|| anyhow::anyhow!("origin {origin} must be http or https"))
}

fn origin_of(url: &Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    Some(format!("{}://{}:{port}", url.scheme(), host))
}

/// Run a component that imports `soil:http` and exports `run(args)`.
pub struct HttpRuntime {
    engine: Engine,
    linker: Linker<HttpHost>,
}

impl HttpRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(anyhow::Error::from)
            .context("link wasi:io polling")?;
        http::add_to_linker::<HttpHost, wasmtime::component::HasSelf<HttpHost>>(
            &mut linker,
            |host| host,
        )?;
        Ok(Self { engine, linker })
    }

    pub fn run_component(&self, bytes: &[u8], host: HttpHost, args: &[String]) -> Result<()> {
        let component = Component::new(&self.engine, bytes)?;
        let mut store = Store::new(&self.engine, host);
        let bindings = Runner::instantiate(&mut store, &component, &self.linker)?;
        bindings
            .call_run(&mut store, args)?
            .map_err(|error| anyhow::anyhow!("guest: {error}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_host::soil::http::http::Host as _;
    use std::{io::Write as _, net::TcpListener, sync::mpsc, thread};

    struct StaticCredentials;
    impl CredentialProvider for StaticCredentials {
        fn resolve(&self, handle: &str) -> Result<Option<(String, String)>> {
            Ok((handle == "test").then(|| ("authorization".into(), "Bearer spike".into())))
        }
    }

    fn serve_once(
        body: &'static str,
        expected_auth: Option<&'static str>,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let text = read_request(&mut stream);
            tx.send(text.clone()).unwrap();
            if let Some(expected) = expected_auth {
                assert!(
                    text.to_ascii_lowercase()
                        .contains(&format!("authorization: {expected}").to_ascii_lowercase())
                );
            }
            stream
                .write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}", body.len(), body).as_bytes())
                .unwrap();
        });
        (format!("http://{addr}"), rx, handle)
    }

    /// Read one request, headers and body. A write may arrive in more than one
    /// packet, so a single `read` is not enough to see its body.
    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..end]);
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + length {
                break;
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    #[test]
    fn fetches_allowlisted_origin_and_bounds_body() {
        let (origin, _rx, handle) = serve_once("hello", None);
        let policy = HttpPolicy::new([origin.clone()], 1024, Duration::from_secs(5)).unwrap();
        let mut host = HttpHost::new(policy);
        let response = host
            .fetch(http::Request {
                url: format!("{origin}/feed.xml"),
                method: http::Method::Get,
                headers: vec![http::Header {
                    name: "accept".into(),
                    value: "text/xml".into(),
                }],
                body: None,
                credential: None,
                max_bytes: 128,
                timeout_ms: 2_000,
            })
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
        handle.join().unwrap();
    }

    #[test]
    fn denies_unlisted_origin_and_injects_credentials() {
        let (origin, rx, handle) = serve_once("ok", Some("Bearer spike"));
        let policy = HttpPolicy::new([origin.clone()], 1024, Duration::from_secs(5)).unwrap();
        let mut host = HttpHost::new(policy).with_credentials(Arc::new(StaticCredentials));
        let response = host
            .fetch(http::Request {
                url: format!("{origin}/private.xml"),
                method: http::Method::Get,
                headers: vec![],
                body: None,
                credential: Some("test".into()),
                max_bytes: 128,
                timeout_ms: 2_000,
            })
            .unwrap();
        assert_eq!(response.status, 200);
        assert!(
            rx.recv()
                .unwrap()
                .to_ascii_lowercase()
                .contains("authorization: bearer spike")
        );
        handle.join().unwrap();

        let mut denied = HttpHost::new(
            HttpPolicy::new(
                vec!["https://example.com".to_string()],
                1024,
                Duration::from_secs(5),
            )
            .unwrap(),
        );
        assert_eq!(
            denied
                .fetch(http::Request {
                    url: "https://not-allowed.invalid/".into(),
                    method: http::Method::Get,
                    headers: vec![],
                    body: None,
                    credential: None,
                    max_bytes: 128,
                    timeout_ms: 2_000,
                })
                .unwrap_err(),
            http::Error::Denied
        );
    }

    #[test]
    fn put_carries_a_json_body_to_the_origin() {
        let (origin, rx, handle) = serve_once("{}", None);
        let policy = HttpPolicy::new([origin.clone()], 1024, Duration::from_secs(5)).unwrap();
        let mut host = HttpHost::new(policy);
        let response = host
            .fetch(http::Request {
                url: format!("{origin}/api/key/groups/0/action"),
                method: http::Method::Put,
                headers: vec![http::Header {
                    name: "content-type".into(),
                    value: "application/json".into(),
                }],
                body: Some(br#"{"on":true}"#.to_vec()),
                credential: None,
                max_bytes: 128,
                timeout_ms: 2_000,
            })
            .unwrap();
        assert_eq!(response.status, 200);
        let request = rx.recv().unwrap();
        assert!(
            request.starts_with("PUT /api/key/groups/0/action"),
            "{request}"
        );
        assert!(request.contains(r#"{"on":true}"#), "{request}");
        handle.join().unwrap();
    }

    #[test]
    fn the_provider_is_preferred_and_only_unsupported_falls_back() {
        struct Always(Result<TransportResponse, TransportError>);
        impl HttpTransport for Always {
            fn fetch(
                &self,
                _request: &TransportRequest,
            ) -> Result<TransportResponse, TransportError> {
                self.0.clone()
            }
        }

        let request = TransportRequest {
            url: Url::parse("https://example.com/").unwrap(),
            method: http::Method::Get,
            headers: vec![],
            body: None,
            timeout: Duration::from_secs(1),
            max_bytes: 64,
        };
        let answer = |from: &'static str| {
            Arc::new(Always(Ok(TransportResponse {
                status: 200,
                final_url: from.into(),
                headers: vec![],
                body: from.as_bytes().to_vec(),
            }))) as Arc<dyn HttpTransport>
        };

        let prefer = PreferProvider::new(answer("page"), answer("node"));
        assert_eq!(prefer.fetch(&request).unwrap().body, b"page");

        let absent = PreferProvider::new(
            Arc::new(Always(Err(TransportError::Unsupported))),
            answer("node"),
        );
        assert_eq!(absent.fetch(&request).unwrap().body, b"node");

        // A refusal is an answer, not an absence: it must reach the caller
        // rather than quietly routing around the provider.
        let denied = PreferProvider::new(
            Arc::new(Always(Err(TransportError::Denied))),
            answer("node"),
        );
        assert_eq!(denied.fetch(&request).unwrap_err(), TransportError::Denied);
    }

    #[test]
    fn a_body_on_a_read_is_rejected() {
        let mut host = HttpHost::new(
            HttpPolicy::new(
                vec!["https://example.com".to_string()],
                1024,
                Duration::from_secs(5),
            )
            .unwrap(),
        );
        assert_eq!(
            host.fetch(http::Request {
                url: "https://example.com/".into(),
                method: http::Method::Get,
                headers: vec![],
                body: Some(b"unexpected".to_vec()),
                credential: None,
                max_bytes: 128,
                timeout_ms: 2_000,
            })
            .unwrap_err(),
            http::Error::Invalid
        );
    }

    #[test]
    fn an_oversized_body_is_rejected_before_the_transport() {
        let mut host = HttpHost::new(
            HttpPolicy::new(
                vec!["https://example.com".to_string()],
                8,
                Duration::from_secs(5),
            )
            .unwrap(),
        );
        assert_eq!(
            host.fetch(http::Request {
                url: "https://example.com/".into(),
                method: http::Method::Post,
                headers: vec![],
                body: Some(b"more than eight bytes".to_vec()),
                credential: None,
                max_bytes: 8,
                timeout_ms: 2_000,
            })
            .unwrap_err(),
            http::Error::OverBudget
        );
    }
}
