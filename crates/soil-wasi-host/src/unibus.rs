//! `soil:unibus`: bounded poll and publish over the Unibus.
//!
//! The WIT contract is opaque bytes in, opaque bytes out. A Spore names a
//! kind and supplies or expects a JSON-object body; the host owns the Unibus
//! session, the credential, hub selection, and the retention window. There is
//! no raw socket, no peer id, and no token at this boundary.
//!
//! `soil:unibus` is the poll/publish edge for Spores that observe as well as
//! publish. It is the only publication surface; `fungos:events` was
//! retired in its favor.
//!
//! # Sparse hubs
//!
//! A single `ClientConfig::hub_endpoint` gives a single hub. Compact fleets
//! (pris, batty, studio, agora-one, titan) instead configure an ordered
//! endpoint list and use [`UnibusBus::connect_any`]: the adapter tries the
//! list in order and records which hub answered. That covers a client that
//! picks the nearest healthy hub, not hub-to-hub federation, which stays in
//! the Unibus router (gossip/relay) rather than in this WIT.
//!
//! When no hub is healthy, both operations fail with `unavailable`. That is
//! deliberate: the Spore sees a typed failure and the caller can retry,
//! whereas a silent local queue would hide a dead fleet. Confluence callers
//! map `unavailable` to HTTP 503 so the provider retries the delivery.
//!
//! `unavailable` is transient, though, not terminal: [`UnibusBus`] owns a
//! supervisor thread that rebuilds a dead session with exponential backoff and
//! ordered hub failover, so a hub restart costs a few failed publishes rather
//! than a restart of every Spore in the fleet. The contract does not change -
//! while the session is down, `publish` and `poll` still return `unavailable`.
//!
//! Retention is split by backend: [`MemoryBus`] is a bounded retained log
//! with an index cursor for tests, while [`UnibusBus`] is the live Unibus
//! tail, so any non-empty cursor is `stale` and the Spore restarts live.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use wasmtime::component::{HasSelf, Linker};

use crate::host::WasiHost;

wasmtime::component::bindgen!({
    path: "../../wit/soil-unibus.wit",
    world: "unibus-host",
});

use soil::unibus::bus as api;

/// The generated `soil:unibus` WIT trait and value types, for hosts that
/// compose the WASI profile with their own store state.
pub use soil::unibus::bus;

/// Largest body a Spore may publish or observe through this profile.
///
/// Matches the Unibus SDK control-plane ceiling (`MAX_BODY_BYTES`). Larger
/// payloads belong on the content plane and should be published as a
/// reference (CID/URL) in the JSON body, not as bulk bytes.
pub const MAX_BUS_BODY: usize = 64 * 1024;

/// Longest `kind` we accept before handing it to the transport. The Unibus
/// SDK already enforces 1..128 bytes; the host enforces the same bound so a
/// Spore error is `invalid` rather than a transport panic.
pub const MAX_KIND_BYTES: usize = 128;

/// Hard ceiling for `poll(limit)`. A Spore must name what it observes, and the
/// host must be able to serve the page in one WIT call.
pub const MAX_POLL_LIMIT: u32 = 256;

/// Host-side TTL ceiling: 24 hours. `ttl_ms == 0` means "host default".
pub const MAX_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// One retained or live frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: String,
    /// Producer that published the frame. Presentation, not authorization.
    pub producer: String,
    pub issued_at_ms: u64,
    /// Opaque JSON-object bytes. The Spore owns the schema.
    pub body: Vec<u8>,
}

/// One page of frames plus the opaque cursor for the next page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub frames: Vec<Frame>,
    pub next: Option<String>,
}

/// Acceptance of one publish. The transport request id is not a dedupe
/// claim: callers that need exactly-once must dedupe on `(kind, body)`
/// content (Confluence does so on `provider` + `delivery_id`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusReceipt {
    pub request_id: String,
}

/// Typed bus failure. Mirrors `soil:unibus/error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusError {
    Denied,
    Invalid,
    OverBudget,
    Stale,
    Unavailable,
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied => f.write_str("bus operation denied"),
            Self::Invalid => f.write_str("invalid bus request"),
            Self::OverBudget => f.write_str("bus request over budget"),
            Self::Stale => f.write_str("bus cursor predates retention"),
            Self::Unavailable => f.write_str("bus unavailable"),
        }
    }
}

impl std::error::Error for BusError {}

/// Host-side bus transport.
///
/// One bus serves every Spore invocation in a store. The adapter owns the
/// Unibus session and the hub endpoint; the guest never sees them.
pub trait Bus: Send + Sync {
    fn publish(
        &self,
        kind: &str,
        body: &[u8],
        destination: Option<&str>,
        ttl_ms: u64,
    ) -> std::result::Result<BusReceipt, BusError>;

    fn poll(
        &self,
        kinds: &[String],
        cursor: Option<&str>,
        limit: u32,
    ) -> std::result::Result<Page, BusError>;
}

fn validate_publish(
    kind: &str,
    body: &[u8],
    destination: Option<&str>,
    ttl_ms: u64,
) -> std::result::Result<(), BusError> {
    if kind.is_empty() || kind.trim().is_empty() || kind.len() > MAX_KIND_BYTES {
        return Err(BusError::Invalid);
    }
    if body.len() > MAX_BUS_BODY {
        return Err(BusError::OverBudget);
    }
    if body.is_empty() {
        return Err(BusError::Invalid);
    }
    if let Some(destination) = destination
        && (destination.is_empty() || destination.len() > MAX_KIND_BYTES)
    {
        return Err(BusError::Invalid);
    }
    if ttl_ms > MAX_TTL_MS {
        return Err(BusError::Invalid);
    }
    Ok(())
}

fn validate_poll(kinds: &[String], limit: u32) -> std::result::Result<(), BusError> {
    if kinds.is_empty() {
        // The WIT is explicit: an empty list means "no kinds", never "all".
        return Err(BusError::Invalid);
    }
    if kinds
        .iter()
        .any(|kind| kind.trim().is_empty() || kind.len() > MAX_KIND_BYTES)
    {
        return Err(BusError::Invalid);
    }
    if limit == 0 {
        return Err(BusError::Invalid);
    }
    if limit > MAX_POLL_LIMIT {
        return Err(BusError::OverBudget);
    }
    Ok(())
}

/// Decode an opaque body for the Unibus SDK.
///
/// The SDK's `Publish.body` is a JSON `Value`; the SDK's own worker then
/// flattens the object into the envelope `Message`. Non-object JSON is
/// rejected here so the guest gets `invalid` instead of a flattened
/// `"value"` wrapper that consumers would have to guess at.
fn body_as_json_object(body: &[u8]) -> std::result::Result<serde_json::Value, BusError> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| BusError::Invalid)?;
    if !value.is_object() {
        return Err(BusError::Invalid);
    }
    Ok(value)
}

/// In-memory bus for tests and local proofs.
///
/// Retains every published frame up to a bound and paginates with a base-10
/// index cursor. A cursor at or past the tail returns an empty page; a
/// cursor past the head after truncation is `stale`. There is no hub, no
/// authentication, and no persistence: a restart loses the log. That is
/// intentional so tests cannot mistake it for the transport.
#[derive(Default)]
pub struct MemoryBus {
    inner: std::sync::Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    frames: Vec<Frame>,
    /// Absolute index of `frames[0]` in the logical log. Lets the cursor
    /// stay meaningful after truncation.
    base: u64,
    next: u64,
}

/// Largest retained prefix. Beyond this the oldest frames are dropped and
/// cursors that pointed into the dropped prefix report `stale`.
pub const MAX_RETAINED_FRAMES: usize = 1_000;

impl MemoryBus {
    /// Every retained frame, chronologically.
    pub fn frames(&self) -> Vec<Frame> {
        self.inner
            .lock()
            .map(|state| state.frames.clone())
            .unwrap_or_default()
    }
}

impl Bus for MemoryBus {
    fn publish(
        &self,
        kind: &str,
        body: &[u8],
        destination: Option<&str>,
        ttl_ms: u64,
    ) -> std::result::Result<BusReceipt, BusError> {
        validate_publish(kind, body, destination, ttl_ms)?;
        // MemoryBus is the test double: it retains JSON-object bodies.
        let _ = body_as_json_object(body)?;
        let mut state = self.inner.lock().map_err(|_| BusError::Unavailable)?;
        state.next += 1;
        let frame = Frame {
            kind: kind.to_string(),
            producer: "local".to_string(),
            issued_at_ms: crate::context::now_ms(),
            body: body.to_vec(),
        };
        state.frames.push(frame);
        while state.frames.len() > MAX_RETAINED_FRAMES {
            state.frames.remove(0);
            state.base += 1;
        }
        Ok(BusReceipt {
            request_id: format!("local-{}", state.next),
        })
    }

    fn poll(
        &self,
        kinds: &[String],
        cursor: Option<&str>,
        limit: u32,
    ) -> std::result::Result<Page, BusError> {
        validate_poll(kinds, limit)?;
        let state = self.inner.lock().map_err(|_| BusError::Unavailable)?;
        // None/empty means "from the head of the retained window", not index 0
        // once the head has moved after truncation.
        let start = match cursor {
            None | Some("") => state.base,
            Some(raw) => raw.parse::<u64>().map_err(|_| BusError::Invalid)?,
        };
        if start < state.base {
            return Err(BusError::Stale);
        }
        let offset = usize::try_from(start - state.base).map_err(|_| BusError::Stale)?;
        if offset > state.frames.len() {
            return Err(BusError::Stale);
        }
        let limit = limit as usize;
        let mut frames = Vec::new();
        let mut index = start;
        for frame in state.frames.iter().skip(offset) {
            if !kinds.iter().any(|kind| kind == &frame.kind) {
                index += 1;
                continue;
            }
            if frames.len() >= limit {
                break;
            }
            frames.push(frame.clone());
            index += 1;
        }
        let next = if index < state.base + state.frames.len() as u64 {
            // More retained frames remain beyond this page.
            Some(index.to_string())
        } else {
            None
        };
        Ok(Page { frames, next })
    }
}

/// How a dead hub session is rebuilt.
///
/// A hub restart is seconds and a power cut is minutes, so the first retry is
/// fast and the ceiling is low: a recovered hub must not wait a minute to be
/// noticed, and a fleet of Spores must not turn one dead hub into a thundering
/// herd. The jitter below spreads them out further.
#[derive(Clone, Copy, Debug)]
struct Retry {
    first_ms: u64,
    max_ms: u64,
    tick_ms: u64,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            first_ms: 500,
            max_ms: 30_000,
            tick_ms: 200,
        }
    }
}

/// The live session, plus everything needed to build another one.
///
/// Shared with the supervisor thread, which is the only place a session is
/// created or dropped after startup. `publish` and `poll` only ever borrow the
/// current one, so a caller never waits for a reconnect.
struct Link {
    base: unibus::client::ClientConfig,
    endpoints: Vec<String>,
    retry: Retry,
    live: Mutex<Option<unibus_sdk::Session>>,
    endpoint: Mutex<String>,
    down_since: Mutex<Option<Instant>>,
    stopped: AtomicBool,
}

impl Link {
    fn connect(&self, endpoint: &str) -> anyhow::Result<unibus_sdk::Session> {
        let mut config = self.base.clone();
        config.hub_endpoint = endpoint.to_owned();
        unibus_sdk::Session::connect(config)
            .with_context(|| format!("connect unibus hub {endpoint}"))
    }

    /// Remove a session that has died, and record when that happened.
    ///
    /// The SDK's worker thread exits on the first I/O error and flips
    /// `connected` false, so this is noticed within its read timeout. Dropping
    /// the session joins that thread, so it happens outside the lock.
    fn take_dead(&self) -> bool {
        let dead = self
            .live
            .lock()
            .ok()
            .and_then(|live| {
                live.as_ref()
                    .is_some_and(|session| !session.status().connected)
                    .then_some(())
            })
            .is_some();
        if !dead {
            return false;
        }
        let session = self.live.lock().ok().and_then(|mut live| live.take());
        drop(session);
        if let Ok(mut down) = self.down_since.lock()
            && down.is_none()
        {
            *down = Some(Instant::now());
        }
        true
    }

    fn install(&self, session: unibus_sdk::Session, endpoint: &str) {
        let downtime = self
            .down_since
            .lock()
            .ok()
            .and_then(|mut down| down.take())
            .map(|since| since.elapsed());
        if let Ok(mut live) = self.live.lock() {
            *live = Some(session);
        }
        if let Ok(mut current) = self.endpoint.lock() {
            *current = endpoint.to_owned();
        }
        match downtime {
            Some(downtime) => tracing::info!(
                hub = %endpoint,
                downtime_ms = downtime.as_millis(),
                "unibus hub re-registered after an outage"
            ),
            None => tracing::info!(hub = %endpoint, "unibus hub registered"),
        }
    }

    fn is_live(&self) -> bool {
        self.live.lock().map(|live| live.is_some()).unwrap_or(false)
    }
}

/// Delay before the next attempt: double, capped.
fn next_delay(current: u64, retry: Retry) -> u64 {
    current
        .saturating_mul(2)
        .clamp(retry.first_ms, retry.max_ms)
}

/// Spread a delay by +/-20%, deterministically per endpoint.
///
/// Every Spore in a fleet watches the same hub, so a plain backoff makes them
/// all come back in the same millisecond. Seeding from the endpoint keeps the
/// spread stable across restarts without a random number generator, and keeps
/// it testable.
fn jittered(delay_ms: u64, endpoint: &str) -> Duration {
    let spread = (delay_ms / 5).max(1);
    let seed = endpoint
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    let offset = seed % (spread * 2 + 1);
    Duration::from_millis(delay_ms - spread + offset)
}

/// Sleep in slices, so a stop is noticed promptly rather than after a whole
/// backoff period.
fn wait(link: &Link, total: Duration) -> bool {
    let slice = Duration::from_millis(link.retry.tick_ms.max(1));
    let mut slept = Duration::ZERO;
    while slept < total {
        if link.stopped.load(Ordering::Acquire) {
            return false;
        }
        let step = slice.min(total - slept);
        std::thread::sleep(step);
        slept += step;
    }
    !link.stopped.load(Ordering::Acquire)
}

/// Rebuild a dead session, forever, until the bus is dropped.
///
/// This is the one place a session is created after startup. The shape mirrors
/// Mycelium's peer sync loop: a fixed tick, a per-endpoint attempt with a delay
/// that doubles and caps, a reset on success, and no attempt while the link is
/// healthy.
fn supervise(link: Arc<Link>) {
    let mut index = 0usize;
    let mut delay = link.retry.first_ms;
    let mut next_attempt = Instant::now();
    while !link.stopped.load(Ordering::Acquire) {
        link.take_dead();
        if !link.is_live() && Instant::now() >= next_attempt {
            let endpoint = link.endpoints[index % link.endpoints.len()].clone();
            match link.connect(&endpoint) {
                Ok(session) => {
                    link.install(session, &endpoint);
                    index = 0;
                    delay = link.retry.first_ms;
                    next_attempt = Instant::now();
                }
                Err(error) => {
                    index = index.wrapping_add(1);
                    delay = next_delay(delay, link.retry);
                    next_attempt = Instant::now() + jittered(delay, &endpoint);
                    tracing::warn!(
                        hub = %endpoint,
                        retry_in_ms = delay,
                        hubs_left = link.endpoints.len() - (index % link.endpoints.len()),
                        "unibus hub unreachable; will retry: {error:#}"
                    );
                }
            }
        }
        if !wait(&link, Duration::from_millis(link.retry.tick_ms.max(1))) {
            return;
        }
    }
}

/// Unibus-backed bus.
///
/// Wraps one [`unibus_sdk::Session`] behind [`Bus`]. The session's worker
/// thread owns the TCP connection, registration and heartbeat; `publish` is
/// a non-blocking enqueue that succeeds when queued, not when the router has
/// fanned out. `poll` drains the inbound queue without waiting.
///
/// A session that dies is rebuilt in the background: the supervisor thread
/// retries with exponential backoff, walking the endpoint list on each failure
/// so ordered hub failover is a runtime property rather than only a startup
/// one. Until a new session registers, `publish` and `poll` return
/// `unavailable` - the Spore must never believe it published into a dead fleet.
/// What changes is that the outage ends without restarting the Spore.
pub struct UnibusBus {
    link: Arc<Link>,
    worker: Option<JoinHandle<()>>,
}

impl UnibusBus {
    /// Connect to the hub named by `config.hub_endpoint`.
    pub fn connect(config: unibus::client::ClientConfig) -> anyhow::Result<Self> {
        let endpoint = config.hub_endpoint.clone();
        Self::connect_any(&config, &[endpoint])
    }

    /// Connect from a JSON `ClientConfig` file.
    ///
    /// `token_file` is resolved relative to the config, so a hub config and its
    /// token travel together; the token is read here and never enters the
    /// Spore or the process environment.
    pub fn from_config_file(path: &Path) -> anyhow::Result<Self> {
        let mut config: unibus::client::ClientConfig = serde_json::from_slice(
            &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        config.resolve_token_file(path);
        Self::connect(config)
    }

    /// Connect by trying `endpoints` in order, cloning `base` with each
    /// `hub_endpoint` in turn.
    ///
    /// The first endpoint that registers wins; its address is recorded on
    /// the bus. Endpoints are host addresses (for example `"192.168.1.39:18799"`
    /// for pris, a Tailscale address for studio/agora-one, or `"127.0.0.1:18799"`
    /// for a local `unibus-router`). Order is priority: put the nearest hub
    /// first; the background supervisor walks the same list when a hub dies.
    /// If every endpoint fails, the error names the whole list so an operator
    /// can see the fleet was sparse, not misconfigured.
    pub fn connect_any(
        base: &unibus::client::ClientConfig,
        endpoints: &[String],
    ) -> anyhow::Result<Self> {
        Self::connect_any_with_retry(base, endpoints, Retry::default())
    }

    fn connect_any_with_retry(
        base: &unibus::client::ClientConfig,
        endpoints: &[String],
        retry: Retry,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !endpoints.is_empty(),
            "at least one hub endpoint is required"
        );
        let link = Arc::new(Link {
            base: base.clone(),
            endpoints: endpoints.to_vec(),
            retry,
            live: Mutex::new(None),
            endpoint: Mutex::new(endpoints[0].clone()),
            down_since: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });
        let mut last_error = None;
        for endpoint in endpoints {
            match link.connect(endpoint) {
                Ok(session) => {
                    link.install(session, endpoint);
                    let supervised = Arc::clone(&link);
                    let worker = std::thread::Builder::new()
                        .name("unibus-reconnect".into())
                        .spawn(move || supervise(supervised))
                        .context("spawn the unibus reconnect supervisor")?;
                    return Ok(Self {
                        link,
                        worker: Some(worker),
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

    /// The hub endpoint this session registered against.
    ///
    /// Not stable for the life of the bus: a reconnect that fails over records
    /// the hub that actually answered.
    pub fn hub_endpoint(&self) -> String {
        self.link
            .endpoint
            .lock()
            .map(|endpoint| endpoint.clone())
            .unwrap_or_default()
    }

    /// Session health snapshot for logging.
    ///
    /// `connected` goes false the moment the session dies and stays false until
    /// a new session registers; `dropped_events` is the SDK's count of frames
    /// this Spore refused (overflow, or bytes on the content plane).
    pub fn status(&self) -> unibus_sdk::Status {
        self.link
            .live
            .lock()
            .ok()
            .and_then(|live| live.as_ref().map(unibus_sdk::Session::status))
            .unwrap_or(unibus_sdk::Status {
                connected: false,
                dropped_events: 0,
            })
    }
}

impl Drop for UnibusBus {
    fn drop(&mut self) {
        self.link.stopped.store(true, Ordering::Release);
        // Start no new attempt; the supervisor notices within one tick.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Bus for UnibusBus {
    fn publish(
        &self,
        kind: &str,
        body: &[u8],
        destination: Option<&str>,
        ttl_ms: u64,
    ) -> std::result::Result<BusReceipt, BusError> {
        validate_publish(kind, body, destination, ttl_ms)?;
        let body = body_as_json_object(body)?;
        let live = self.link.live.lock().map_err(|_| BusError::Unavailable)?;
        let Some(session) = live.as_ref() else {
            return Err(BusError::Unavailable);
        };
        if !session.status().connected {
            return Err(BusError::Unavailable);
        }
        let publish = unibus_sdk::Publish {
            destination: destination.map(str::to_string),
            kind: kind.to_string(),
            body,
        };
        // The SDK validates kind/body/destination again; any failure at this
        // point is a transport outage, not a guest shape error. TTL has no
        // SDK knob: the router enforces its own freshness window, so the
        // host validates the range but does not thread it through.
        let request_id = session
            .publish(publish)
            .map_err(|_| BusError::Unavailable)?;
        Ok(BusReceipt { request_id })
    }

    fn poll(
        &self,
        kinds: &[String],
        cursor: Option<&str>,
        limit: u32,
    ) -> std::result::Result<Page, BusError> {
        validate_poll(kinds, limit)?;
        match cursor {
            None | Some("") => {}
            // The live Unibus tail has no retention. A Spore that holds a
            // cursor from a previous live read must restart from the tail.
            Some(_) => return Err(BusError::Stale),
        }
        let live = self.link.live.lock().map_err(|_| BusError::Unavailable)?;
        let Some(session) = live.as_ref() else {
            return Err(BusError::Unavailable);
        };
        if !session.status().connected {
            return Err(BusError::Unavailable);
        }
        let limit = limit as usize;
        let mut frames = Vec::new();
        while frames.len() < limit {
            let Some(event) = session.poll() else {
                break;
            };
            if !kinds.iter().any(|kind| kind == &event.kind) {
                continue;
            }
            let body = serde_json::to_vec(&event.body).map_err(|_| BusError::Unavailable)?;
            frames.push(Frame {
                kind: event.kind,
                producer: event.producer_id,
                issued_at_ms: event.issued_at_unix_ms,
                body,
            });
        }
        Ok(Page { frames, next: None })
    }
}

impl api::Host for WasiHost {
    fn publish(
        &mut self,
        kind: String,
        body: Vec<u8>,
        destination: Option<String>,
        ttl_ms: u64,
    ) -> std::result::Result<api::Receipt, api::Error> {
        let bus = self.bus().ok_or(api::Error::Unavailable)?;
        let receipt = bus
            .publish(&kind, &body, destination.as_deref(), ttl_ms)
            .map_err(map_error)?;
        Ok(api::Receipt {
            request_id: receipt.request_id,
        })
    }

    fn poll(
        &mut self,
        kinds: Vec<String>,
        cursor: Option<String>,
        limit: u32,
    ) -> std::result::Result<api::Page, api::Error> {
        let bus = self.bus().ok_or(api::Error::Unavailable)?;
        let page = bus
            .poll(&kinds, cursor.as_deref(), limit)
            .map_err(map_error)?;
        Ok(api::Page {
            frames: page
                .frames
                .into_iter()
                .map(|frame| api::Frame {
                    kind: frame.kind,
                    producer: frame.producer,
                    issued_at_ms: frame.issued_at_ms,
                    body: frame.body,
                })
                .collect(),
            next: page.next,
        })
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

fn map_error(error: BusError) -> api::Error {
    match error {
        BusError::Denied => api::Error::Denied,
        BusError::Invalid => api::Error::Invalid,
        BusError::OverBudget => api::Error::OverBudget,
        BusError::Stale => api::Error::Stale,
        BusError::Unavailable => api::Error::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(_kind: &str, body: &str) -> Vec<u8> {
        serde_json::json!({ "value": body })
            .to_string()
            .into_bytes()
    }
    #[test]
    fn memory_bus_publishes_and_pages_in_order() {
        let bus = MemoryBus::default();
        let first = bus
            .publish("email.delta.v1", &frame("a", "1"), None, 0)
            .unwrap();
        assert_eq!(first.request_id, "local-1");
        bus.publish("chat.message.v1", &frame("b", "2"), None, 0)
            .unwrap();
        bus.publish("email.delta.v1", &frame("c", "3"), None, 0)
            .unwrap();

        let all = bus.poll(&["email.delta.v1".to_string()], None, 10).unwrap();
        assert_eq!(all.frames.len(), 2);
        assert_eq!(all.frames[0].kind, "email.delta.v1");
        assert_eq!(all.frames[0].producer, "local");

        let page = bus.poll(&["email.delta.v1".to_string()], None, 1).unwrap();
        assert_eq!(page.frames.len(), 1);
        let cursor = page.next.expect("one more email frame remains");
        let rest = bus
            .poll(&["email.delta.v1".to_string()], Some(&cursor), 10)
            .unwrap();
        assert_eq!(rest.frames.len(), 1);
        assert_eq!(rest.next, None);
    }

    #[test]
    fn memory_bus_validates_against_the_wit() {
        let bus = MemoryBus::default();
        // An empty kind list means "no kinds", never "all".
        assert_eq!(bus.poll(&[], None, 1).unwrap_err(), BusError::Invalid);
        assert_eq!(
            bus.poll(&["k".to_string()], None, 0).unwrap_err(),
            BusError::Invalid
        );
        assert_eq!(
            bus.poll(&["k".to_string()], None, MAX_POLL_LIMIT + 1)
                .unwrap_err(),
            BusError::OverBudget
        );
        assert_eq!(
            bus.publish("", &frame("a", "1"), None, 0).unwrap_err(),
            BusError::Invalid
        );
        assert_eq!(
            bus.publish("k", b"not json", None, 0).unwrap_err(),
            BusError::Invalid
        );
        assert_eq!(
            bus.publish("k", br"[1,2,3]", None, 0).unwrap_err(),
            BusError::Invalid
        );
        assert_eq!(
            bus.publish("k", &frame("a", "1"), Some(""), 0).unwrap_err(),
            BusError::Invalid
        );
        assert_eq!(
            bus.publish("k", &frame("a", "1"), None, MAX_TTL_MS + 1)
                .unwrap_err(),
            BusError::Invalid
        );
        assert_eq!(
            bus.publish("k", &vec![b' '; MAX_BUS_BODY + 1], None, 0)
                .unwrap_err(),
            BusError::OverBudget
        );
    }

    #[test]
    fn memory_bus_cursor_reports_stale_after_truncation() {
        // Lower the bound by filling past it; the head cursor goes stale.
        let bus = MemoryBus::default();
        for i in 0..(MAX_RETAINED_FRAMES + 5) {
            bus.publish(
                "k.v1",
                &serde_json::json!({ "i": i }).to_string().into_bytes(),
                None,
                0,
            )
            .unwrap();
        }
        assert_eq!(
            bus.poll(&["k.v1".to_string()], Some("0"), 1).unwrap_err(),
            BusError::Stale
        );
        let tail = bus.poll(&["k.v1".to_string()], None, 1).unwrap();
        assert_eq!(tail.frames.len(), 1);
        assert_eq!(
            bus.poll(&["k.v1".to_string()], Some("999999"), 1)
                .unwrap_err(),
            BusError::Stale
        );
        assert_eq!(
            bus.poll(&["k.v1".to_string()], Some("not-a-number"), 1)
                .unwrap_err(),
            BusError::Invalid
        );
    }

    #[test]
    fn a_backoff_doubles_then_caps() {
        let retry = Retry {
            first_ms: 500,
            max_ms: 30_000,
            tick_ms: 10,
        };
        assert_eq!(next_delay(500, retry), 1_000);
        assert_eq!(next_delay(16_000, retry), 30_000);
        // It never shrinks, and it never waits longer than the ceiling.
        assert_eq!(next_delay(30_000, retry), 30_000);
        assert_eq!(next_delay(u64::MAX, retry), 30_000);
    }

    #[test]
    fn jitter_spreads_a_fleet_without_leaving_the_delay() {
        let delay = 1_000;
        let spread = delay / 5;
        // Same endpoint, same wait: a restart does not reshuffle the fleet.
        assert_eq!(
            jittered(delay, "10.0.0.1:18799"),
            jittered(delay, "10.0.0.1:18799")
        );
        for endpoint in ["pris:18799", "batty:18799", "studio:18799", "127.0.0.1:1"] {
            let wait = jittered(delay, endpoint).as_millis() as u64;
            assert!(
                (delay - spread..=delay + spread).contains(&wait),
                "{endpoint} waited {wait}ms, outside {delay}±{spread}"
            );
        }
    }

    /// A bound hub listener and a token file the client config can point at.
    ///
    /// The listener is handed back rather than rebound on the same port: a
    /// rebinding race would make this test flaky for reasons that have nothing
    /// to do with reconnecting.
    fn fake_hub() -> (std::net::TcpListener, String, std::path::PathBuf) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let token =
            std::env::temp_dir().join(format!("soil-unibus-token-{}-{stamp}", std::process::id()));
        std::fs::write(&token, b"transport-token").expect("write token");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a hub");
        let endpoint = listener.local_addr().expect("addr").to_string();
        (listener, endpoint, token)
    }

    fn client_config(endpoint: &str, token: &std::path::Path) -> unibus::client::ClientConfig {
        serde_json::from_value(serde_json::json!({
            "installation_id": "soil-test",
            "producer_id": "confluence",
            "hub_endpoint": endpoint,
            "token_file": token,
            "zones": ["test"],
            "schema_version": 2
        }))
        .expect("client config")
    }

    fn accept_and_register(
        listener: &std::net::TcpListener,
        producer: &str,
    ) -> std::net::TcpStream {
        let (mut socket, _) = listener.accept().expect("accept a client");
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let (registration, _) =
            unibus::framing::read_frame(&mut socket).expect("read the registration frame");
        let reply = unibus::Envelope::new(
            "soil-test",
            "router",
            Some(producer),
            "transport-token",
            unibus::Message::new(
                unibus::kinds::V1_REPLY,
                serde_json::json!({
                    "request_id": registration.request_id,
                    "success": true
                }),
            ),
        );
        unibus::framing::write_frame(&mut socket, &reply, &[])
            .expect("write the registration reply");
        socket
    }

    fn wait_until(what: &str, deadline: Duration, mut check: impl FnMut() -> bool) {
        let until = Instant::now() + deadline;
        while Instant::now() < until {
            if check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn a_lost_session_is_rebuilt_and_re_registers() {
        use std::sync::mpsc;

        let (listener, endpoint, token) = fake_hub();
        // Handshakes are driven by the test, not by sleeps: a hub that dies and
        // returns on a timer would make a slow machine look like a bug.
        let (kill_first, when_to_die) = mpsc::channel::<()>();
        let (re_registered, second_registration) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let first = accept_and_register(&listener, "confluence");
            when_to_die
                .recv()
                .expect("the test decides when the hub dies");
            drop(first);
            // A reconnect must re-register, not resume a dead socket. A retry
            // that gave up mid-handshake is expected, so keep accepting until
            // one completes.
            loop {
                match std::panic::catch_unwind(|| accept_and_register(&listener, "confluence")) {
                    Ok(second) => {
                        re_registered.send(()).expect("report the re-registration");
                        // Stay up until the test is finished with the bus.
                        let (_keep_open, until_done) = mpsc::channel::<()>();
                        let _ = until_done.recv();
                        drop(second);
                        return;
                    }
                    Err(_) => continue,
                }
            }
        });

        let bus = UnibusBus::connect_any_with_retry(
            &client_config(&endpoint, &token),
            std::slice::from_ref(&endpoint),
            Retry {
                first_ms: 20,
                max_ms: 40,
                tick_ms: 5,
            },
        )
        .expect("the first registration succeeds");
        assert!(
            bus.status().connected,
            "the hub registered the first session"
        );

        kill_first.send(()).expect("kill the hub connection");
        // A write is what discovers a dead socket: the session reports itself
        // down when a send fails, not when the peer closes, so keep publishing
        // while waiting. This is the contract the Spore actually lives with.
        wait_until(
            "the session to report itself down",
            Duration::from_secs(10),
            || {
                let _ = bus.publish("test.v1", &frame("test.v1", "{}"), None, 0);
                !bus.status().connected
            },
        );
        // Fail closed while down: the Spore must not think it published.
        assert_eq!(
            bus.publish("test.v1", &frame("test.v1", "{}"), None, 0)
                .unwrap_err(),
            BusError::Unavailable
        );

        second_registration
            .recv_timeout(Duration::from_secs(10))
            .expect("the supervisor must dial the hub again and re-register");
        wait_until(
            "the rebuilt session to report itself up",
            Duration::from_secs(10),
            || bus.status().connected,
        );
        assert!(
            bus.publish("test.v1", &frame("test.v1", "{}"), None, 0)
                .is_ok(),
            "publishing resumes once the hub is back"
        );
        assert_eq!(
            bus.hub_endpoint(),
            endpoint,
            "a single-endpoint fleet stays on its hub"
        );
        let _ = std::fs::remove_file(token);
    }

    #[test]
    fn a_hub_that_never_answers_never_blocks_a_caller() {
        let (listener, endpoint, token) = fake_hub();
        // Nothing is listening by the time we dial: connect must fail at
        // startup rather than spin or guess.
        drop(listener);
        let refused = UnibusBus::connect_any(
            &client_config(&endpoint, &token),
            &[endpoint.clone(), "127.0.0.1:1".to_string()],
        );
        let message = format!("{:#}", refused.err().expect("no hub, no bus"));
        assert!(message.contains("all unibus hubs unreachable"), "{message}");
        let _ = std::fs::remove_file(token);
    }
}
