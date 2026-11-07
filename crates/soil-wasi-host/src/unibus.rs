//! `soil:unibus`: bounded poll and publish over the Unibus.
//!
//! The WIT contract is opaque bytes in, opaque bytes out. A Spore names a
//! kind and supplies or expects a JSON-object body; the host owns the Unibus
//! session, the credential, hub selection, and the retention window. There is
//! no raw socket, no peer id, and no token at this boundary.
//!
//! `fungos:events` remains the one-way publication intent with an explicit
//! idempotency key. `soil:unibus` is the poll/publish edge for Spores that
//! observe as well as publish. Hosts may bridge the two: a `fungos:events`
//! Spore can sit on top of the same session a `soil:unibus` Spore observes.
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
//! Retention is split by backend: [`MemoryBus`] is a bounded retained log
//! with an index cursor for tests, while [`UnibusBus`] is the live Unibus
//! tail, so any non-empty cursor is `stale` and the Spore restarts live.

use std::fmt;

use anyhow::Context as _;
use wasmtime::component::{HasSelf, Linker};

use crate::host::WasiHost;

wasmtime::component::bindgen!({
    path: "../../wit/soil-unibus.wit",
    world: "unibus-host",
});

use soil::unibus::bus as api;

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

/// Unibus-backed bus.
///
/// Wraps one [`unibus_sdk::Session`] behind [`Bus`]. The session's worker
/// thread owns the TCP connection, registration and heartbeat; `publish` is
/// a non-blocking enqueue that succeeds when queued, not when the router has
/// fanned out. `poll` drains the inbound queue without waiting.
///
/// There is no automatic reconnect in this adapter: if the session dies,
/// `publish` and `poll` fail with `unavailable` until the host rebuilds the
/// bus. That fail-closed behavior matters for sparse hubs: a Spore must not
/// believe it published into a dead fleet. Reconnect (with backoff and hub
/// failover) belongs in the host that owns the lifecycle, so the bus stays a
/// pure adapter. See [`UnibusBus::connect_any`] for ordered hub failover at
/// connect time.
pub struct UnibusBus {
    session: std::sync::Mutex<unibus_sdk::Session>,
    hub_endpoint: String,
}

impl UnibusBus {
    /// Connect to the hub named by `config.hub_endpoint`.
    pub fn connect(config: unibus::client::ClientConfig) -> anyhow::Result<Self> {
        let hub_endpoint = config.hub_endpoint.clone();
        let session = unibus_sdk::Session::connect(config).context("connect unibus hub")?;
        Ok(Self {
            session: std::sync::Mutex::new(session),
            hub_endpoint,
        })
    }

    /// Connect by trying `endpoints` in order, cloning `base` with each
    /// `hub_endpoint` in turn.
    ///
    /// The first endpoint that registers wins; its address is recorded on
    /// the bus. Endpoints are host addresses (for example `"192.168.1.39:18799"`
    /// for pris, a Tailscale address for studio/agora-one, or `"127.0.0.1:18799"`
    /// for a local `unibus-router`). Order is priority: put the nearest hub
    /// first. If every endpoint fails, the error names the whole list so an
    /// operator can see the fleet was sparse, not misconfigured.
    pub fn connect_any(
        base: &unibus::client::ClientConfig,
        endpoints: &[String],
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !endpoints.is_empty(),
            "at least one hub endpoint is required"
        );
        let mut last_error = None;
        for endpoint in endpoints {
            let mut config = base.clone();
            config.hub_endpoint = endpoint.clone();
            match Self::connect(config) {
                Ok(bus) => return Ok(bus),
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
    pub fn hub_endpoint(&self) -> &str {
        &self.hub_endpoint
    }

    /// Session health snapshot for logging. Sparse-hub hosts poll this on a
    /// housekeeping tick and rebuild the bus when `connected` goes false.
    pub fn status(&self) -> unibus_sdk::Status {
        self.session
            .lock()
            .map(|session| session.status())
            .unwrap_or(unibus_sdk::Status {
                connected: false,
                dropped_events: 0,
            })
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
        let session = self.session.lock().map_err(|_| BusError::Unavailable)?;
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
        let session = self.session.lock().map_err(|_| BusError::Unavailable)?;
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
}
