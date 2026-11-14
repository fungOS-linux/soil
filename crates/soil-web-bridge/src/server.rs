//! The transport: HTTP/1.1 for unary calls, one WebSocket for the session.
//!
//! Why two, and not one of each extreme:
//!
//! - A call plane that is plain HTTP is cacheable, proxyable, curl-able, and
//!   describable by an OpenAPI document. That is worth keeping even though the
//!   session exists.
//! - Server-sent events are a *stream*, not a session: every client message is a
//!   fresh request, so calls cannot be cancelled, and the two directions have no
//!   ordering relative to each other. `soil:*` is full of calls that need both
//!   (`present` then `input` then `close`), so the duplex channel is a
//!   WebSocket. SSE stays where it already works — a read-only feed.
//!
//! Both share one envelope and one dispatcher, so a call over HTTP and a call
//! over the socket run the same grant check and the same code.
//!
//! Concurrency is one thread per connection. That is the right call at this
//! size: a browser has a handful of sessions, and a thread that blocks on
//! `read` is simpler to reason about than a task that has to prove it never
//! holds a lock across an await.

use std::collections::BTreeMap;
use std::io::{self, Cursor, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tungstenite::Message;

use crate::Registry;
use crate::grants::GrantSet;
use crate::protocol::{
    self, Call, CallError, Event, EventFrame, Frame, Inbound, Outcome, Request, Response, VERSION,
};

const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const SESSION_POLL: Duration = Duration::from_millis(50);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SESSIONS: usize = 64;
/// Per-session backlog. A subscriber that falls this far behind loses events
/// rather than blocking a writer; `changed` is a notification, not a delivery.
const EVENT_CHANNEL: usize = 64;
/// Outbound calls queued behind the one a session is already running.
const PROVIDER_QUEUE: usize = 8;
const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 10_000;
const MIN_PROVIDER_TIMEOUT: Duration = Duration::from_millis(50);
const MAX_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);

type Subscribers = Arc<Mutex<Vec<(u64, mpsc::SyncSender<Event>)>>>;
const PAGE: &str = include_str!("page.html");

/// Why a request was refused before it reached a capability.
struct Refusal {
    status: u16,
    reason: &'static str,
}

/// Who may talk to this bridge.
///
/// The real answer is "the gateway, which has already done OIDC or mTLS": a
/// browser cannot hold a client certificate, so identity terminates at the
/// gateway (Mycelium's OIDC, `fpl-auth`, Pocket ID, Okta — the gateway's
/// problem, not this crate's) and the bridge listens on loopback behind it.
///
/// Two local modes remain, and both are honest about what they prove:
///
/// - **Loopback only** — the default. No token, no identity; the OS is the
///   boundary. Right for `soil-terminal-web`-style developer use.
/// - **Bearer token** — for a bridge behind a tunnel, or a test. A token is
///   shared, so it identifies *the deployment*, never a person; never treat one
///   as a principal.
///
/// An origin allowlist is separate because it answers a different question: not
/// *who* may connect, but *which page* may script a connection. A browser sends
/// `Origin` on a WebSocket handshake; a native client does not, and is allowed.
pub struct Auth {
    token: Option<[u8; 32]>,
    origins: Vec<String>,
}

impl Default for Auth {
    fn default() -> Self {
        Self::loopback_only()
    }
}

impl Auth {
    pub fn loopback_only() -> Self {
        Self {
            token: None,
            origins: Vec::new(),
        }
    }

    pub fn with_token(token: &str) -> Self {
        Self {
            token: Some(digest(token)),
            origins: Vec::new(),
        }
    }

    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.origins.push(origin.into());
        self
    }

    fn authorize(&self, head: &Head, peer: SocketAddr) -> Result<(), Refusal> {
        match &self.token {
            Some(expected) => {
                let presented = head
                    .headers
                    .get("authorization")
                    .and_then(|value| value.split_once(' '))
                    .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                    .map(|(_, token)| token.trim())
                    .or_else(|| head.query.get("token").map(String::as_str));
                match presented {
                    Some(token) if digest(token) == *expected => Ok(()),
                    _ => Err(Refusal {
                        status: 401,
                        reason: "Unauthorized",
                    }),
                }
            }
            None if peer.ip().is_loopback() => Ok(()),
            None => Err(Refusal {
                status: 401,
                reason: "Unauthorized",
            }),
        }
    }

    /// The origin a response may echo, if the page is one we allow.
    fn allowed_origin(&self, head: &Head) -> Option<String> {
        let origin = head.headers.get("origin")?;
        if self.origins.iter().any(|allowed| allowed == origin) {
            Some(origin.clone())
        } else {
            None
        }
    }

    fn origin_refused(&self, head: &Head) -> bool {
        if self.origins.is_empty() {
            return false;
        }
        head.headers
            .get("origin")
            .is_some_and(|origin| !self.origins.iter().any(|allowed| allowed == origin))
    }
}

fn digest(value: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

/// One Spore's view of the host.
pub struct Bridge {
    spore: String,
    grants: GrantSet,
    /// What the page is about to run: the manifest projection a browser host
    /// needs to know which imports to provide, and the module itself, so a tab
    /// never has to be trusted with a path.
    package: Option<Arc<SporePackage>>,
    registry: Registry,
    auth: Auth,
    bus: EventBus,
    sessions: AtomicUsize,
    /// package -> (session id, call queue). One provider per package.
    providers: Providers,
    session_seq: AtomicU64,
    /// Sessions that talk HTTP instead of WebSocket (see `handle_events`).
    sse: SseRegistry,
    sse_seq: AtomicU64,
    /// Outbound call ids are `~n`; `~` is outside the client id alphabet, so a
    /// page can never mint an id that collides with the host's.
    server_calls: AtomicU64,
}

/// The package a session is bound to, as a browser host needs it: the
/// manifest projection (id, version, surfaces, grants) and the module bytes.
pub struct SporePackage {
    manifest: serde_json::Value,
    module: Option<Vec<u8>>,
}

impl Bridge {
    pub fn new(spore: impl Into<String>, grants: GrantSet, registry: Registry) -> Self {
        Self {
            spore: spore.into(),
            grants,
            package: None,
            registry,
            auth: Auth::default(),
            bus: EventBus::default(),
            sessions: AtomicUsize::new(0),
            providers: Providers::default(),
            session_seq: AtomicU64::new(1),
            sse: Arc::new(Mutex::new(BTreeMap::new())),
            sse_seq: AtomicU64::new(1),
            server_calls: AtomicU64::new(1),
        }
    }

    /// Attach the package this bridge is bound to, so a browser host can fetch
    /// what it is about to run instead of being told a path.
    pub fn with_package(mut self, manifest: serde_json::Value, module: Option<Vec<u8>>) -> Self {
        self.package = Some(Arc::new(SporePackage {
            manifest,
            module,
        }));
        self
    }

    /// Call into a session that volunteered to implement `package`.
    ///
    /// Synchronous, bounded, and cancellable: the caller waits, the session
    /// thread enforces the same deadline, and a result that arrives after the
    /// deadline is dropped by id. This is the direction a Spore needs when the
    /// node has no network but the browser does.
    pub fn call_provider(
        &self,
        package: &str,
        function: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let timeout = timeout.clamp(MIN_PROVIDER_TIMEOUT, MAX_PROVIDER_TIMEOUT);
        let sender = self
            .providers
            .lock()
            .map_err(|_| CallError::io("the provider table is poisoned"))?
            .get(package)
            .map(|(_, sender)| sender.clone());
        let Some(sender) = sender else {
            return Err(CallError::unavailable(format!(
                "no session provides {package}"
            )));
        };

        let id = format!("~{}", self.server_calls.fetch_add(1, Ordering::Relaxed));
        let (reply, answer) = mpsc::sync_channel(1);
        sender
            .try_send(ProviderCall {
                id,
                package: package.to_owned(),
                function: function.to_owned(),
                params,
                deadline: Instant::now() + timeout,
                reply,
            })
            .map_err(|_| {
                CallError::unavailable(format!("the {package} provider is busy with another call"))
            })?;

        // Wait a little past the session's own deadline so its more specific
        // timeout wins the race.
        match answer.recv_timeout(timeout + Duration::from_millis(250)) {
            Ok(result) => result,
            Err(_) => Err(CallError::timeout(format!(
                "{package}.{function} did not answer in time"
            ))),
        }
    }

    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = auth;
        self
    }

    pub fn spore(&self) -> &str {
        &self.spore
    }

    pub fn grants(&self) -> &GrantSet {
        &self.grants
    }

    pub fn serve(self: Arc<Self>, bind: SocketAddr) -> anyhow::Result<Server> {
        serve(self, bind)
    }

    /// Is a live session answering for `package`?
    ///
    /// A transport asks this before spending a call, so "no provider" is an
    /// `Unsupported` rather than a timeout.
    pub fn provides(&self, package: &str) -> bool {
        self.providers
            .lock()
            .map(|providers| providers.contains_key(package))
            .unwrap_or(false)
    }

    fn dispatch(&self, request: &Request) -> (Response, Vec<Event>) {
        let (outcome, events) = self.registry.dispatch(&self.grants, &request.call);
        let response = Response {
            v: VERSION,
            id: request.call.id.clone(),
            result: outcome,
        };
        (response, events)
    }
}

/// A running bridge. Dropping it stops the accept loop.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn serve(bridge: Arc<Bridge>, bind: SocketAddr) -> anyhow::Result<Server> {
    let listener = TcpListener::bind(bind).with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let handle = thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, peer)) => {
                    // The listener is non-blocking so the accept loop can be
                    // stopped; on macOS a socket accepted from a non-blocking
                    // listener inherits O_NONBLOCK, which would turn every
                    // request read into a would-block race. Hand the connection
                    // thread a blocking socket, explicitly.
                    if let Err(error) = stream.set_nonblocking(false) {
                        eprintln!("soil-web-bridge: could not block {peer}: {error}");
                        continue;
                    }
                    let bridge = bridge.clone();
                    thread::spawn(move || handle_connection(bridge, stream, peer));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL);
                }
                Err(_) => break,
            }
        }
    });
    Ok(Server {
        addr,
        stop,
        handle: Some(handle),
    })
}

fn handle_connection(bridge: Arc<Bridge>, mut stream: TcpStream, peer: SocketAddr) {
    let _ = stream.set_nodelay(true);
    let Ok((buffer, head_len)) = read_head(&stream) else {
        return;
    };
    let Some(head) = Head::parse(&buffer[..head_len]) else {
        respond(
            &mut stream,
            400,
            "Bad Request",
            "text/plain; charset=utf-8",
            b"malformed request\n",
            None,
        );
        return;
    };

    // Read the body before deciding anything. Closing a socket with unread bytes
    // in its receive buffer sends RST instead of FIN, and the client can lose the
    // response it just read — so every path below, including a refusal, has
    // already consumed the request.
    if head.method == "POST" && !head.headers.contains_key("content-length") {
        respond(
            &mut stream,
            411,
            "Length Required",
            "text/plain; charset=utf-8",
            b"POST needs a Content-Length\n",
            None,
        );
        return;
    }
    let body = match read_body(&mut stream, &head, &buffer, head_len) {
        Ok(body) => body,
        Err(BodyError::TooLarge) => {
            respond_json(
                &mut stream,
                413,
                "Payload Too Large",
                "",
                &Outcome::Err(CallError::over_budget(format!(
                    "body is over the {MAX_BODY_BYTES} byte budget"
                ))),
                None,
            );
            return;
        }
        Err(BodyError::Truncated) => {
            respond(
                &mut stream,
                400,
                "Bad Request",
                "text/plain; charset=utf-8",
                b"truncated body\n",
                None,
            );
            return;
        }
    };

    if let Err(refusal) = bridge.auth.authorize(&head, peer) {
        respond(
            &mut stream,
            refusal.status,
            refusal.reason,
            "text/plain; charset=utf-8",
            b"unauthorized\n",
            None,
        );
        return;
    }

    let cors = bridge.auth.allowed_origin(&head);
    match (head.method.as_str(), head.path.as_str()) {
        ("GET", "/") => respond(
            &mut stream,
            200,
            "OK",
            "text/html; charset=utf-8",
            PAGE.as_bytes(),
            cors,
        ),
        ("GET", "/healthz") => respond(
            &mut stream,
            200,
            "OK",
            "text/plain; charset=utf-8",
            b"ok\n",
            cors,
        ),
        ("OPTIONS", _) => respond(&mut stream, 204, "No Content", "text/plain", b"", cors),
        ("POST", "/v1/call") => handle_call(&bridge, &mut stream, &body, cors),
        ("POST", "/v1/provider/call") => handle_provider_call(&bridge, &mut stream, &body, cors),
        ("POST", "/v1/frame") => handle_frame(&bridge, &mut stream, &head, &body, cors),
        ("GET", "/v1/spore") => match bridge.package.as_ref() {
            Some(package) => respond_json(
                &mut stream,
                200,
                "OK",
                "",
                &Outcome::Ok(package.manifest.clone()),
                cors,
            ),
            None => respond(
                &mut stream,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"this bridge was started without a package\n",
                cors,
            ),
        },
        ("GET", "/v1/spore/module.wasm") => {
            match bridge.package.as_ref().and_then(|package| package.module.as_ref()) {
                Some(module) => respond(
                    &mut stream,
                    200,
                    "OK",
                    "application/wasm",
                    module,
                    cors,
                ),
                None => respond(
                    &mut stream,
                    404,
                    "Not Found",
                    "text/plain; charset=utf-8",
                    b"this bridge serves no module for its Spore\n",
                    cors,
                ),
            }
        }
        ("GET", "/v1/events") => handle_events(bridge.clone(), stream, &head, cors),
        ("GET", "/v1/session") => handle_session(bridge.clone(), stream, buffer, &head),
        _ => respond(
            &mut stream,
            404,
            "Not Found",
            "text/plain; charset=utf-8",
            b"not found\n",
            cors,
        ),
    }
}

fn handle_call(bridge: &Bridge, stream: &mut TcpStream, body: &[u8], cors: Option<String>) {
    if body.is_empty() {
        respond_json(
            stream,
            400,
            "Bad Request",
            "",
            &Outcome::Err(CallError::invalid("empty request body")),
            cors,
        );
        return;
    }

    let request: Request = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(error) => {
            respond_json(
                stream,
                400,
                "Bad Request",
                "",
                &Outcome::Err(CallError::invalid(format!("not a bridge request: {error}"))),
                cors,
            );
            return;
        }
    };
    if let Err(error) = request.validate() {
        respond_json(
            stream,
            400,
            "Bad Request",
            &request.call.id,
            &Outcome::Err(error),
            cors,
        );
        return;
    }

    let (response, events) = bridge.dispatch(&request);
    for event in events {
        bridge.bus.publish(event);
    }
    // The id is the client's correlation handle; echoing it is the contract.
    respond_json(
        &mut *stream,
        200,
        "OK",
        &response.id,
        &response.result,
        cors,
    );
}

fn handle_provider_call(
    bridge: &Bridge,
    stream: &mut TcpStream,
    body: &[u8],
    cors: Option<String>,
) {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ProviderRequest {
        package: String,
        function: String,
        #[serde(default)]
        params: Value,
        #[serde(default)]
        timeout_ms: Option<u64>,
    }

    let result = match serde_json::from_slice::<ProviderRequest>(body) {
        Ok(request) => bridge.call_provider(
            &request.package,
            &request.function,
            request.params,
            Duration::from_millis(request.timeout_ms.unwrap_or(DEFAULT_PROVIDER_TIMEOUT_MS)),
        ),
        Err(error) => Err(CallError::invalid(format!("not a provider call: {error}"))),
    };
    respond_json(
        stream,
        200,
        "OK",
        "",
        &match result {
            Ok(value) => Outcome::Ok(value),
            Err(error) => Outcome::Err(error),
        },
        cors,
    );
}

/// Sessions that talk HTTP instead of WebSocket.
///
/// A proxy that strips hop-by-hop headers — the mycelium gateway does — cannot
/// carry an upgrade, and a page behind one would sit at "connecting" forever.
/// The same frames travel as server-sent events out and POSTs in, which every
/// HTTP/1.1 proxy forwards. The WebSocket stays for clients that can open one.
type SseRegistry = Arc<Mutex<BTreeMap<String, Arc<SseSession>>>>;

/// Server calls the page has been given but not answered, by id.
type PendingCalls = Mutex<BTreeMap<String, (Instant, mpsc::SyncSender<Result<Value, CallError>>)>>;

struct SseSession {
    provider_tx: mpsc::SyncSender<ProviderCall>,
    /// Calls the host made: when to give up, and where the answer goes.
    pending: PendingCalls,
    registration: Mutex<Registration>,
}

fn handle_events(bridge: Arc<Bridge>, mut stream: TcpStream, _head: &Head, cors: Option<String>) {
    let id = format!("s{}", bridge.sse_seq.fetch_add(1, Ordering::Relaxed));
    let (event_tx, events) = mpsc::sync_channel(EVENT_CHANNEL);
    let subscription = bridge.bus.subscribe(event_tx);
    let (provider_tx, provider_rx) = mpsc::sync_channel(PROVIDER_QUEUE);
    let session = Arc::new(SseSession {
        provider_tx: provider_tx.clone(),
        pending: Mutex::new(BTreeMap::new()),
        registration: Mutex::new(Registration {
            bridge: bridge.clone(),
            session: id.clone(),
            packages: Vec::new(),
        }),
    });
    if let Ok(mut sessions) = bridge.sse.lock() {
        sessions.insert(id.clone(), session.clone());
    }

    let mut headers = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
         X-Accel-Buffering: no\r\nConnection: close\r\n",
    );
    if let Some(origin) = cors {
        headers.push_str(&format!(
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\n"
        ));
    }
    headers.push_str("\r\n");
    if stream.write_all(headers.as_bytes()).is_err() {
        return;
    }

    // The client has no socket identity to echo, so the first frame names the
    // session it must quote on `POST /v1/frame`.
    let ready = EventFrame {
        v: VERSION,
        event: Event::Ready {
            spore: bridge.spore.clone(),
            grants: bridge.grants.declared(),
            interfaces: bridge.registry.describe(),
            session: Some(id.clone()),
        },
    };
    if send_sse(&mut stream, &ready).is_err() {
        drop(subscription);
        if let Ok(mut sessions) = bridge.sse.lock() {
            sessions.remove(&id);
        }
        return;
    }

    let mut ticks: u32 = 0;
    'stream: loop {
        while let Ok(event) = events.try_recv() {
            if send_sse(&mut stream, &EventFrame { v: VERSION, event }).is_err() {
                break 'stream;
            }
        }

        // A provider call is a server-to-client frame; the answer arrives on a
        // different request, so it is tracked here by id until it does.
        while let Ok(call) = provider_rx.try_recv() {
            let frame = Request {
                v: VERSION,
                call: Call {
                    id: call.id.clone(),
                    interface: call.package.clone(),
                    function: call.function.clone(),
                    params: call.params,
                },
            };
            let Ok(text) = serde_json::to_string(&frame) else {
                continue;
            };
            if write_sse_data(&mut stream, &text).is_err() {
                break 'stream;
            }
            if let Ok(mut pending) = session.pending.lock() {
                pending.insert(call.id, (call.deadline, call.reply));
            }
        }

        for (id, reply) in expire_pending(&session) {
            if write_sse_data(&mut stream, &protocol::cancel_frame(&id)).is_err() {
                break 'stream;
            }
            let _ = reply.send(Err(CallError::timeout("the page did not answer in time")));
        }

        ticks += 1;
        // A comment frame keeps the connection observably open, and a failed
        // write is how this loop notices the page went away.
        if ticks.is_multiple_of(50) && stream.write_all(b": ping\n\n").is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    drop(subscription);
    if let Ok(mut sessions) = bridge.sse.lock() {
        sessions.remove(&id);
    }
}

fn expire_pending(
    session: &SseSession,
) -> Vec<(String, mpsc::SyncSender<Result<Value, CallError>>)> {
    let Ok(mut pending) = session.pending.lock() else {
        return Vec::new();
    };
    let now = Instant::now();
    let expired: Vec<String> = pending
        .iter()
        .filter(|(_, (deadline, _))| *deadline <= now)
        .map(|(id, _)| id.clone())
        .collect();
    expired
        .into_iter()
        .filter_map(|id| pending.remove(&id).map(|(_, reply)| (id, reply)))
        .collect()
}

fn send_sse(stream: &mut TcpStream, frame: &EventFrame) -> io::Result<()> {
    let text = serde_json::to_string(frame)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "encode"))?;
    write_sse_data(stream, &text)
}

fn write_sse_data(stream: &mut TcpStream, text: &str) -> io::Result<()> {
    stream.write_all(format!("data: {text}\n\n").as_bytes())?;
    stream.flush()
}

/// Anything a page sends while its events arrive over SSE.
fn handle_frame(
    bridge: &Bridge,
    stream: &mut TcpStream,
    head: &Head,
    body: &[u8],
    cors: Option<String>,
) {
    let Some(id) = head.query.get("session").cloned() else {
        respond_json(
            stream,
            400,
            "Bad Request",
            "",
            &Outcome::Err(CallError::invalid("POST /v1/frame needs ?session=")),
            cors,
        );
        return;
    };
    let session = bridge
        .sse
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(&id).cloned());
    let Some(session) = session else {
        respond_json(
            stream,
            404,
            "Not Found",
            "",
            &Outcome::Err(CallError::unavailable(format!("no such session: {id}"))),
            cors,
        );
        return;
    };

    let frame: Inbound = match serde_json::from_slice(body) {
        Ok(frame) => frame,
        Err(error) => {
            respond_json(
                stream,
                400,
                "Bad Request",
                "",
                &Outcome::Err(CallError::invalid(format!("not a bridge frame: {error}"))),
                cors,
            );
            return;
        }
    };

    match frame {
        Inbound::Frame(Frame::Call(request)) => {
            let result = match request.validate() {
                Ok(()) => {
                    let (response, events) = bridge.dispatch(&request);
                    for event in events {
                        bridge.bus.publish(event);
                    }
                    response.result
                }
                Err(error) => Outcome::Err(error),
            };
            respond_json(stream, 200, "OK", &request.call.id, &result, cors);
        }
        Inbound::Frame(Frame::Provide(request)) => {
            let result = match request.validate() {
                Ok(()) => {
                    let mut registration = match session.registration.lock() {
                        Ok(registration) => registration,
                        Err(_) => {
                            respond_json(
                                stream,
                                500,
                                "Internal Server Error",
                                &request.provide.id,
                                &Outcome::Err(CallError::io("session state is poisoned")),
                                cors,
                            );
                            return;
                        }
                    };
                    registration
                        .register(request.provide.packages, &session.provider_tx)
                        .map(|packages| serde_json::json!({ "packages": packages }))
                }
                Err(error) => Err(error),
            };
            respond_json(
                stream,
                200,
                "OK",
                &request.provide.id,
                &match result {
                    Ok(value) => Outcome::Ok(value),
                    Err(error) => Outcome::Err(error),
                },
                cors,
            );
        }
        Inbound::Response(response) => {
            let reply = session
                .pending
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&response.id));
            match reply {
                Some((_, reply)) => {
                    let _ = reply.send(match response.result {
                        Outcome::Ok(value) => Ok(value),
                        Outcome::Err(error) => Err(error),
                    });
                    respond_json(
                        stream,
                        200,
                        "OK",
                        &response.id,
                        &Outcome::Ok(serde_json::json!({ "delivered": true })),
                        cors,
                    );
                }
                None => respond_json(
                    stream,
                    404,
                    "Not Found",
                    &response.id,
                    &Outcome::Err(CallError::not_found(format!(
                        "no call in flight with id {}",
                        response.id
                    ))),
                    cors,
                ),
            }
        }
    }
}

fn handle_session(bridge: Arc<Bridge>, stream: TcpStream, buffer: Vec<u8>, head: &Head) {
    let upgrading = head
        .headers
        .get("upgrade")
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if !upgrading {
        let text = "this endpoint is a WebSocket; send Upgrade: websocket";
        let mut stream = stream;
        respond(
            &mut stream,
            426,
            "Upgrade Required",
            "text/plain; charset=utf-8",
            text.as_bytes(),
            None,
        );
        return;
    }
    if bridge.auth.origin_refused(head) {
        let mut stream = stream;
        respond(
            &mut stream,
            403,
            "Forbidden",
            "text/plain; charset=utf-8",
            b"origin is not allowed\n",
            None,
        );
        return;
    }
    if bridge.sessions.load(Ordering::Relaxed) >= MAX_SESSIONS {
        let mut stream = stream;
        respond(
            &mut stream,
            503,
            "Service Unavailable",
            "text/plain; charset=utf-8",
            b"too many sessions\n",
            None,
        );
        return;
    }

    // The read timeout is what lets one thread own both directions: wait briefly
    // for a client frame, then drain events between reads. Nothing is lost — a
    // frame that timed out mid-read stays buffered inside tungstenite.
    //
    // The handshake gets a generous timeout and the session gets the short one:
    // a client that stalls mid-handshake must not hold a thread forever, and a
    // 50 ms budget during the handshake turns a slow packet into a failed
    // upgrade rather than a slow one.
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let stream = PrefixedStream {
        head: Cursor::new(buffer),
        stream,
    };
    let mut socket = match tungstenite::accept(stream) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("soil-web-bridge: session handshake failed: {error}");
            return;
        }
    };
    if let Err(error) = socket.get_mut().stream.set_read_timeout(Some(SESSION_POLL)) {
        eprintln!("soil-web-bridge: session timeout not set: {error}");
        return;
    }
    bridge.sessions.fetch_add(1, Ordering::Relaxed);
    let _guard = SessionGuard(&bridge.sessions);

    let session_id = format!("w{}", bridge.session_seq.fetch_add(1, Ordering::Relaxed));
    let (event_tx, events) = mpsc::sync_channel(EVENT_CHANNEL);
    let subscription = bridge.bus.subscribe(event_tx);
    let (provider_tx, provider_rx) = mpsc::sync_channel(PROVIDER_QUEUE);

    Session {
        bridge: bridge.clone(),
        socket,
        events,
        provider_rx,
        provider_tx,
        registration: Registration {
            bridge: bridge.clone(),
            session: session_id,
            packages: Vec::new(),
        },
    }
    .serve();
    drop(subscription);
}

/// One outbound call waiting on a session thread.
///
/// The reply channel is what makes the host's call synchronous: the caller
/// blocks on it with its own timeout, and the session thread enforces the same
/// deadline. Whichever fires first, a late answer is dropped by id.
struct ProviderCall {
    id: String,
    package: String,
    function: String,
    params: Value,
    deadline: Instant,
    reply: mpsc::SyncSender<Result<Value, CallError>>,
}

type Providers = Arc<Mutex<BTreeMap<String, (String, mpsc::SyncSender<ProviderCall>)>>>;

/// What a session has registered, removed when the session ends.
///
/// Owns the bridge rather than borrowing it: an SSE session is created by one
/// request, used by a later one, and dropped by a third.
struct Registration {
    bridge: Arc<Bridge>,
    session: String,
    packages: Vec<String>,
}

impl Registration {
    /// Register packages this session will answer for.
    ///
    /// Two rules, both from the manifest: the Spore must have asked for the
    /// package, and one session answers per package. Silently replacing a live
    /// provider would break the caller's expectation that its next call lands
    /// where the last one did.
    fn register(
        &mut self,
        packages: Vec<String>,
        sender: &mpsc::SyncSender<ProviderCall>,
    ) -> Result<Vec<String>, CallError> {
        let mut accepted = Vec::new();
        for package in packages {
            if !self.bridge.grants.covers(&package) {
                return Err(CallError::denied(format!(
                    "{package} is not granted for this Spore"
                )));
            }
            if !self.packages.contains(&package) {
                let mut providers = self
                    .bridge
                    .providers
                    .lock()
                    .map_err(|_| CallError::io("provider table is poisoned"))?;
                if let Some((holder, _)) = providers.get(&package)
                    && *holder != self.session
                {
                    return Err(CallError::denied(format!(
                        "{package} is already provided by another session"
                    )));
                }
                providers.insert(package.clone(), (self.session.clone(), sender.clone()));
                self.packages.push(package.clone());
            }
            accepted.push(package);
        }
        Ok(accepted)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let Ok(mut providers) = self.bridge.providers.lock() else {
            return;
        };
        for package in &self.packages {
            if providers
                .get(package)
                .is_some_and(|(holder, _)| *holder == self.session)
            {
                providers.remove(package);
            }
        }
    }
}

struct Session {
    bridge: Arc<Bridge>,
    socket: tungstenite::WebSocket<PrefixedStream>,
    events: mpsc::Receiver<Event>,
    provider_rx: mpsc::Receiver<ProviderCall>,
    provider_tx: mpsc::SyncSender<ProviderCall>,
    registration: Registration,
}

impl Session {
    fn serve(mut self) {
        let ready = EventFrame {
            v: VERSION,
            event: Event::Ready {
                spore: self.bridge.spore.clone(),
                grants: self.bridge.grants.declared(),
                interfaces: self.bridge.registry.describe(),
                session: None,
            },
        };
        if self.send_event(&ready).is_err() {
            return;
        }

        loop {
            if self.pump_events().is_err() {
                return;
            }

            // A provider call is handled inline: the session thread owns the
            // socket, and a call in flight has to keep reading to see its own
            // answer. One call at a time per session is the price of that
            // simplicity, and it is written down in the README.
            if let Ok(call) = self.provider_rx.try_recv()
                && self.run_provider_call(call).is_err()
            {
                return;
            }

            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let inbound = serde_json::from_str::<Inbound>(text.as_str());
                    match inbound {
                        Ok(inbound) => {
                            if self.handle(inbound).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            let response = Response {
                                v: VERSION,
                                id: String::new(),
                                result: Outcome::Err(CallError::invalid(format!(
                                    "not a bridge frame: {error}"
                                ))),
                            };
                            if self.send_response(&response).is_err() {
                                return;
                            }
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                Ok(Message::Binary(_)) | Ok(Message::Frame(_)) => break,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => {
                    eprintln!("soil-web-bridge: session ended: {error}");
                    break;
                }
            }
        }

        let _ = self.socket.close(None);
    }

    fn pump_events(&mut self) -> Result<(), ()> {
        while let Ok(event) = self.events.try_recv() {
            self.send_event(&EventFrame { v: VERSION, event })?;
        }
        Ok(())
    }

    fn send_event(&mut self, frame: &EventFrame) -> Result<(), ()> {
        let text = serde_json::to_string(frame).map_err(|_| ())?;
        self.socket.send(Message::text(text)).map_err(|_| ())
    }

    fn send_response(&mut self, response: &Response) -> Result<(), ()> {
        let text = serde_json::to_string(response).map_err(|_| ())?;
        self.socket.send(Message::text(text)).map_err(|_| ())
    }

    fn handle(&mut self, inbound: Inbound) -> Result<(), ()> {
        match inbound {
            Inbound::Frame(Frame::Call(request)) => {
                let response = match request.validate() {
                    Ok(()) => {
                        let (response, events) = self.bridge.dispatch(&request);
                        for event in events {
                            self.bridge.bus.publish(event);
                        }
                        response
                    }
                    Err(error) => Response {
                        v: VERSION,
                        id: request.call.id,
                        result: Outcome::Err(error),
                    },
                };
                self.send_response(&response)
            }
            Inbound::Frame(Frame::Provide(request)) => {
                let result = match request.validate() {
                    Ok(()) => self
                        .registration
                        .register(request.provide.packages, &self.provider_tx)
                        .map(|packages| serde_json::json!({ "packages": packages })),
                    Err(error) => Err(error),
                };
                self.send_response(&Response {
                    v: VERSION,
                    id: request.provide.id,
                    result: match result {
                        Ok(value) => Outcome::Ok(value),
                        Err(error) => Outcome::Err(error),
                    },
                })
            }
            // A late answer to a call the host already gave up on.
            Inbound::Response(_) => Ok(()),
        }
    }

    fn run_provider_call(&mut self, call: ProviderCall) -> Result<(), ()> {
        let request = Request {
            v: VERSION,
            call: Call {
                id: call.id.clone(),
                interface: call.package.clone(),
                function: call.function.clone(),
                params: call.params,
            },
        };
        let text = match serde_json::to_string(&request) {
            Ok(text) => text,
            Err(_) => {
                let _ = call
                    .reply
                    .send(Err(CallError::io("could not encode the call")));
                return Ok(());
            }
        };
        if self.socket.send(Message::text(text)).is_err() {
            let _ = call
                .reply
                .send(Err(CallError::unavailable("provider session ended")));
            return Err(());
        }

        loop {
            if Instant::now() >= call.deadline {
                let _ = self
                    .socket
                    .send(Message::text(protocol::cancel_frame(&call.id)));
                let _ = call.reply.send(Err(CallError::timeout(format!(
                    "{} did not answer in time",
                    call.package
                ))));
                return Ok(());
            }
            if self.pump_events().is_err() {
                let _ = call
                    .reply
                    .send(Err(CallError::unavailable("provider session ended")));
                return Err(());
            }
            match self.socket.read() {
                Ok(Message::Text(text)) => match serde_json::from_str::<Inbound>(text.as_str()) {
                    Ok(Inbound::Response(response)) if response.id == call.id => {
                        let reply = match response.result {
                            Outcome::Ok(value) => Ok(value),
                            Outcome::Err(error) => Err(error),
                        };
                        let _ = call.reply.send(reply);
                        return Ok(());
                    }
                    Ok(inbound) => {
                        if self.handle(inbound).is_err() {
                            let _ = call
                                .reply
                                .send(Err(CallError::unavailable("provider session ended")));
                            return Err(());
                        }
                    }
                    Err(_) => {}
                },
                Ok(Message::Close(_)) | Ok(Message::Binary(_)) | Ok(Message::Frame(_)) => {
                    let _ = call
                        .reply
                        .send(Err(CallError::unavailable("provider session ended")));
                    return Err(());
                }
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => {
                    let _ = call
                        .reply
                        .send(Err(CallError::unavailable("provider session ended")));
                    return Err(());
                }
            }
        }
    }
}

struct SessionGuard<'a>(&'a AtomicUsize);

impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A stream that replays bytes already read while parsing the request head, and
/// then behaves like the socket. The WebSocket handshake has to see the head it
/// was already sent, and `std::io::Chain` cannot write.
struct PrefixedStream {
    head: Cursor<Vec<u8>>,
    stream: TcpStream,
}

impl Read for PrefixedStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.head.read(buf)?;
        if read == 0 {
            self.stream.read(buf)
        } else {
            Ok(read)
        }
    }
}

impl Write for PrefixedStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn read_head(mut stream: &TcpStream) -> io::Result<(Vec<u8>, usize)> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            return Ok((buffer, position + 4));
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head is over budget",
            ));
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before the request head",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

enum BodyError {
    TooLarge,
    Truncated,
}

/// Consume the request body.
///
/// Bounded by [`MAX_BODY_BYTES`]; no chunked encoding, so a request either states
/// its length or has no body. Reading it here rather than in each handler is the
/// fix for the interesting failure: a server that responds and closes while the
/// peer's bytes are still unread triggers RST, and the client may never see the
/// response it caused.
fn read_body(
    stream: &mut TcpStream,
    head: &Head,
    buffer: &[u8],
    head_len: usize,
) -> Result<Vec<u8>, BodyError> {
    let Some(length) = head
        .headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return Ok(Vec::new());
    };
    if length > MAX_BODY_BYTES {
        return Err(BodyError::TooLarge);
    }
    let mut body = buffer[head_len..].to_vec();
    body.truncate(length);
    while body.len() < length {
        let mut chunk = [0u8; 8192];
        let want = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => break,
            Ok(read) => body.extend_from_slice(&chunk[..read]),
            Err(_) => break,
        }
    }
    if body.len() != length {
        return Err(BodyError::Truncated);
    }
    Ok(body)
}

struct Head {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
}

impl Head {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.split("\r\n");
        let mut request = lines.next()?.split(' ');
        let method = request.next()?.to_ascii_uppercase();
        let target = request.next()?;
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path, parse_query(query)),
            None => (target, BTreeMap::new()),
        };
        let mut headers = BTreeMap::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            let (name, value) = line.split_once(':')?;
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
        Some(Self {
            method,
            path: path.to_owned(),
            query,
            headers,
        })
    }
}

fn parse_query(query: &str) -> BTreeMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    cors: Option<String>,
) {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(origin) = cors {
        head.push_str(&format!(
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Headers: authorization, content-type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n"
        ));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn respond_json(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    id: &str,
    result: &Outcome,
    cors: Option<String>,
) {
    let body = serde_json::to_vec(&Response {
        v: VERSION,
        id: id.to_owned(),
        result: result.clone(),
    })
    .unwrap_or_else(|_| {
        br#"{"v":1,"id":"","result":{"err":{"code":"io","message":"encode"}}}"#.to_vec()
    });
    respond(stream, status, reason, "application/json", &body, cors);
}

/// Fan-out for host events.
///
/// A slow subscriber loses events rather than blocking a write. That is safe
/// because `changed` is a notification, not a delivery: a client that missed one
/// re-scans, and a client that received one still has to read the record.
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Subscribers,
    next: Arc<AtomicU64>,
}

impl EventBus {
    fn subscribe(&self, sender: mpsc::SyncSender<Event>) -> Subscription {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut subscribers) = self.inner.lock() {
            subscribers.push((id, sender));
        }
        Subscription {
            bus: self.clone(),
            id,
        }
    }

    fn publish(&self, event: Event) {
        let Ok(mut subscribers) = self.inner.lock() else {
            return;
        };
        subscribers.retain(|(_, sender)| {
            !matches!(
                sender.try_send(event.clone()),
                Err(mpsc::TrySendError::Disconnected(_))
            )
        });
    }
}

pub struct Subscription {
    bus: EventBus,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Ok(mut subscribers) = self.bus.inner.lock() {
            subscribers.retain(|(id, _)| *id != self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_head_parses_into_a_path_and_lowercase_headers() {
        let head = Head::parse(
            b"POST /v1/call?token=abc HTTP/1.1\r\nHost: x\r\nOrigin: https://a\r\n\r\n",
        )
        .unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/v1/call");
        assert_eq!(head.query.get("token").map(String::as_str), Some("abc"));
        assert_eq!(
            head.headers.get("origin").map(String::as_str),
            Some("https://a")
        );
    }

    #[test]
    fn an_allowlisted_origin_is_echoed_and_another_is_refused() {
        let auth = Auth::loopback_only().allow_origin("https://canvas.myc");
        let allowed = Head::parse(b"GET / HTTP/1.1\r\nOrigin: https://canvas.myc\r\n\r\n").unwrap();
        let other = Head::parse(b"GET / HTTP/1.1\r\nOrigin: https://evil.example\r\n\r\n").unwrap();
        assert_eq!(
            auth.allowed_origin(&allowed).as_deref(),
            Some("https://canvas.myc")
        );
        assert!(auth.allowed_origin(&other).is_none());
        assert!(!auth.origin_refused(&allowed));
        assert!(auth.origin_refused(&other));
    }

    #[test]
    fn a_token_is_required_when_one_is_configured() {
        let auth = Auth::with_token("s3cret");
        let peer: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let bearer =
            Head::parse(b"GET / HTTP/1.1\r\nAuthorization: Bearer s3cret\r\n\r\n").unwrap();
        let missing = Head::parse(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        assert!(auth.authorize(&bearer, peer).is_ok());
        assert!(auth.authorize(&missing, peer).is_err());
    }

    #[test]
    fn a_session_without_the_bearer_can_use_a_query_token() {
        let auth = Auth::with_token("s3cret");
        let peer: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let query = Head::parse(b"GET /v1/session?token=s3cret HTTP/1.1\r\n\r\n").unwrap();
        assert!(auth.authorize(&query, peer).is_ok());
    }
}
