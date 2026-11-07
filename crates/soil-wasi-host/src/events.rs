//! `fungos:events`: publication intent with host-owned transport.
//!
//! The guest names a fact and supplies a JSON body. The host decides whether
//! the kind is granted, enforces the size ceiling, deduplicates by idempotency
//! key, and hands the event to its sink (Unibus today). The guest never sees a
//! broker address, token, session, or subscription.

use std::fmt;

use wasmtime::component::{HasSelf, Linker};

use crate::host::WasiHost;

wasmtime::component::bindgen!({
    path: "../../wit/fungos-events.wit",
    world: "host",
});

use fungos::events::publisher as api;

/// Largest event body a Spore may publish through this profile.
pub const MAX_EVENT_BODY: usize = 64 * 1024;

/// A fact a Spore observed and wants published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: String,
    /// JSON object bytes. The sink validates the schema for the kind.
    pub body: Vec<u8>,
    pub idempotency_key: Option<String>,
}

/// The transport's acceptance of one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub request_id: String,
    pub duplicate: bool,
}

/// Typed publication failure. Mirrors `fungos:events/error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    Denied,
    Invalid,
    OverBudget,
    Unavailable,
}

impl fmt::Display for EventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied => f.write_str("event kind denied"),
            Self::Invalid => f.write_str("invalid event"),
            Self::OverBudget => f.write_str("event over budget"),
            Self::Unavailable => f.write_str("event backend unavailable"),
        }
    }
}

impl std::error::Error for EventError {}

/// Host-side event transport.
///
/// One sink serves every Spore invocation in a store. The host adapter owns
/// the Unibus session, not the guest. Implementations must be idempotent for a
/// repeated key when they report `duplicate`.
pub trait EventSink: Send + Sync {
    fn publish(&self, event: &Event) -> std::result::Result<Receipt, EventError>;
}

impl api::Host for WasiHost {
    fn publish(&mut self, event: api::Event) -> std::result::Result<api::Receipt, api::Error> {
        let sink = self.event_sink().ok_or(api::Error::Unavailable)?;
        if event.body.len() > MAX_EVENT_BODY {
            return Err(api::Error::OverBudget);
        }
        if event.kind.trim().is_empty() {
            return Err(api::Error::Invalid);
        }
        let receipt = sink
            .publish(&Event {
                kind: event.kind,
                body: event.body,
                idempotency_key: event.idempotency_key,
            })
            .map_err(map_error)?;
        Ok(api::Receipt {
            request_id: receipt.request_id,
            duplicate: receipt.duplicate,
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

fn map_error(error: EventError) -> api::Error {
    match error {
        EventError::Denied => api::Error::Denied,
        EventError::Invalid => api::Error::Invalid,
        EventError::OverBudget => api::Error::OverBudget,
        EventError::Unavailable => api::Error::Unavailable,
    }
}

/// In-memory sink for tests and local proofs.
///
/// It records every accepted event and deduplicates by idempotency key. It is
/// deliberately not a production transport: a restart loses the record and the
/// dedupe table, which is why the real adapter is Unibus.
#[derive(Default)]
pub struct MemoryEventSink {
    inner: std::sync::Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    events: Vec<Event>,
    seen: std::collections::HashMap<String, String>,
    next: u64,
}

impl MemoryEventSink {
    pub fn events(&self) -> Vec<Event> {
        self.inner
            .lock()
            .map(|state| state.events.clone())
            .unwrap_or_default()
    }
}

impl EventSink for MemoryEventSink {
    fn publish(&self, event: &Event) -> std::result::Result<Receipt, EventError> {
        let mut state = self.inner.lock().map_err(|_| EventError::Unavailable)?;
        if let Some(key) = &event.idempotency_key
            && let Some(request_id) = state.seen.get(key)
        {
            return Ok(Receipt {
                request_id: request_id.clone(),
                duplicate: true,
            });
        }
        state.next += 1;
        let request_id = format!("local-{}", state.next);
        if let Some(key) = &event.idempotency_key {
            state.seen.insert(key.clone(), request_id.clone());
        }
        state.events.push(event.clone());
        Ok(Receipt {
            request_id,
            duplicate: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{WasiHostBuilder, WasiHostPolicy};

    fn event() -> api::Event {
        api::Event {
            kind: "confluence.webhook.received.v1".to_string(),
            body: br#"{"provider":"github"}"#.to_vec(),
            idempotency_key: Some("delivery-1".to_string()),
        }
    }

    #[test]
    fn publishes_and_deduplicates_by_idempotency_key() {
        let sink = Arc::new(MemoryEventSink::default());
        let mut host = WasiHostBuilder::new(WasiHostPolicy::default())
            .event_sink(sink.clone())
            .build()
            .unwrap();
        let first = api::Host::publish(&mut host, event()).unwrap();
        assert!(!first.duplicate);
        let second = api::Host::publish(&mut host, event()).unwrap();
        assert!(second.duplicate);
        assert_eq!(first.request_id, second.request_id);
        assert_eq!(sink.events().len(), 1);
    }

    #[test]
    fn fails_closed_without_a_sink_or_for_a_bad_kind() {
        let mut host = WasiHostBuilder::new(WasiHostPolicy::default())
            .build()
            .unwrap();
        assert_eq!(
            api::Host::publish(&mut host, event()).unwrap_err(),
            api::Error::Unavailable
        );

        let mut host = WasiHostBuilder::new(WasiHostPolicy::default())
            .event_sink(Arc::new(MemoryEventSink::default()))
            .build()
            .unwrap();
        let mut empty = event();
        empty.kind = "  ".to_string();
        assert_eq!(
            api::Host::publish(&mut host, empty).unwrap_err(),
            api::Error::Invalid
        );

        let mut large = event();
        large.body = vec![b' '; MAX_EVENT_BODY + 1];
        assert_eq!(
            api::Host::publish(&mut host, large).unwrap_err(),
            api::Error::OverBudget
        );
    }
}
