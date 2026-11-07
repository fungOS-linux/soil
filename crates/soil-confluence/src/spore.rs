//! Host adapter for the Confluence Spore.
//!
//! The Spore verifies the provider signature and normalizes the delivery; it
//! never sees the tenant, the Unibus session, or a token. This adapter stamps
//! the host-selected tenant, validates the event shape, and publishes exactly
//! one fact per delivery. Two transports share the same policy:
//!
//! * [`ConfluenceEventSink`] for the interim `fungos:events` Spore, which
//!   carries an explicit idempotency key and a `duplicate` receipt.
//! * [`ConfluenceBus`] for `soil:unibus`, which deduplicates on
//!   `provider` + `delivery_id` content rather than a separate key field.
//!
//! Both collapse redelivery to a single Unibus fact. They differ only in how
//! the receipt the guest sees is shaped. Keep both until the Spore fixture
//! moves to `soil:unibus`; then retire the `fungos:events` Spore.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use soil_wasi_host::{Bus, BusError, BusReceipt, Event, EventError, EventSink, Receipt};

use crate::{ConfluenceEvent, EVENT_KIND, MAX_BODY, Publisher};

/// Unibus-backed sink for `confluence.webhook.received.v1`.
pub struct ConfluenceEventSink {
    tenant: String,
    publisher: Arc<dyn Publisher>,
    seen: Mutex<HashMap<String, String>>,
}

impl ConfluenceEventSink {
    pub fn new(tenant: impl Into<String>, publisher: Arc<dyn Publisher>) -> Self {
        Self {
            tenant: tenant.into(),
            publisher,
            seen: Mutex::new(HashMap::new()),
        }
    }
}

/// Parse, validate, and tenant-stamp a Spore body. Shared by both transports.
///
/// Returns the deduplication key (`provider` + `delivery_id`) and the stamped
/// event. The guest `tenant`, if present, is overwritten: tenant is host
/// policy, not a guest claim.
fn stamp_and_validate(
    tenant: &str,
    body: &[u8],
) -> std::result::Result<(String, ConfluenceEvent), ConfluencePolicyError> {
    let mut body: Value =
        serde_json::from_slice(body).map_err(|_| ConfluencePolicyError::Invalid)?;
    let object = body.as_object_mut().ok_or(ConfluencePolicyError::Invalid)?;
    for field in [
        "provider",
        "delivery_id",
        "event_type",
        "received_at_ms",
        "headers",
        "body",
    ] {
        if !object.contains_key(field) {
            return Err(ConfluencePolicyError::Invalid);
        }
    }
    // Tenant is host policy. A guest value, if any, is overwritten rather
    // than trusted.
    object.insert("tenant".to_string(), json!(tenant));
    let event: ConfluenceEvent =
        serde_json::from_value(body).map_err(|_| ConfluencePolicyError::Invalid)?;
    Ok((format!("{}:{}", event.provider, event.delivery_id), event))
}

#[derive(Debug)]
enum ConfluencePolicyError {
    Invalid,
}

impl std::fmt::Display for ConfluencePolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid confluence event")
    }
}

impl std::error::Error for ConfluencePolicyError {}

impl From<ConfluencePolicyError> for EventError {
    fn from(_: ConfluencePolicyError) -> Self {
        EventError::Invalid
    }
}

impl From<ConfluencePolicyError> for BusError {
    fn from(_: ConfluencePolicyError) -> Self {
        BusError::Invalid
    }
}

/// Check the `seen` map for a duplicate, inserting `request_id` on first
/// publish. Shared by both transports; only the receipt shape differs.
fn dedupe(
    seen: &Mutex<HashMap<String, String>>,
    key: &str,
    publish: impl FnOnce() -> std::result::Result<String, String>,
) -> std::result::Result<Option<String>, String> {
    let mut guard = seen.lock().map_err(|_| "unavailable".to_string())?;
    if let Some(request_id) = guard.get(key) {
        return Ok(Some(request_id.clone()));
    }
    let request_id = publish()?;
    if guard.len() >= 10_000 {
        guard.clear();
    }
    guard.insert(key.to_string(), request_id.clone());
    Ok(None)
}

impl EventSink for ConfluenceEventSink {
    fn publish(&self, event: &Event) -> Result<Receipt, EventError> {
        if event.kind != EVENT_KIND {
            return Err(EventError::Denied);
        }
        if event.body.len() > MAX_BODY {
            return Err(EventError::OverBudget);
        }
        let Some(key) = event.idempotency_key.clone() else {
            return Err(EventError::Invalid);
        };
        let (content_key, event) = stamp_and_validate(&self.tenant, &event.body)?;
        // `fungos:events` carries an explicit key; the content key and the
        // guest key must agree. A mismatch is a guest bug, not a retry.
        if key != content_key {
            return Err(EventError::Invalid);
        }
        let publisher = Arc::clone(&self.publisher);
        let event_clone = event.clone();
        let outcome = dedupe(&self.seen, &key, || {
            publisher.publish(&event_clone).map_err(|error| {
                tracing::warn!(%error, provider = %event_clone.provider, "confluence publish failed");
                "unavailable".to_string()
            })
        })
        .map_err(|_| EventError::Unavailable)?;
        if let Some(request_id) = outcome {
            return Ok(Receipt {
                request_id,
                duplicate: true,
            });
        }
        // The first publish inserted the request id; recover it for the receipt.
        let seen = self.seen.lock().map_err(|_| EventError::Unavailable)?;
        let request_id = seen.get(&key).cloned().ok_or(EventError::Unavailable)?;
        Ok(Receipt {
            request_id,
            duplicate: false,
        })
    }
}

/// `soil:unibus` policy for `confluence.webhook.received.v1`.
///
/// Wraps any inner [`Bus`] (a [`soil_wasi_host::UnibusBus`] in production,
/// a [`soil_wasi_host::MemoryBus`] in tests) and adds tenant stamping plus
/// delivery deduplication. `soil:unibus` has no idempotency key, so the key
/// is `provider` + `delivery_id` parsed from the body. `destination` must be
/// empty: Confluence events are fan-out facts, not addressed to one peer.
/// `poll` is delegated to the inner bus so the same host can serve observer
/// Spores without a second session.
pub struct ConfluenceBus {
    tenant: String,
    inner: Arc<dyn Bus>,
    seen: Mutex<HashMap<String, String>>,
}

impl ConfluenceBus {
    /// Wrap `inner` with the Confluence tenant policy.
    pub fn new(tenant: impl Into<String>, inner: Arc<dyn Bus>) -> Self {
        Self {
            tenant: tenant.into(),
            inner,
            seen: Mutex::new(HashMap::new()),
        }
    }
}

impl Bus for ConfluenceBus {
    fn publish(
        &self,
        kind: &str,
        body: &[u8],
        destination: Option<&str>,
        ttl_ms: u64,
    ) -> std::result::Result<BusReceipt, BusError> {
        if kind != EVENT_KIND {
            return Err(BusError::Denied);
        }
        if body.len() > MAX_BODY {
            return Err(BusError::OverBudget);
        }
        if destination.is_some_and(|d| !d.is_empty()) {
            return Err(BusError::Invalid);
        }
        let (key, event) = stamp_and_validate(&self.tenant, body).map_err(BusError::from)?;
        let stamped = serde_json::to_vec(&event).map_err(|_| BusError::Invalid)?;
        let inner = Arc::clone(&self.inner);
        let outcome = dedupe(&self.seen, &key, || {
            inner
                .publish(EVENT_KIND, &stamped, None, ttl_ms)
                .map(|receipt| receipt.request_id)
                .map_err(|error| {
                    tracing::warn!(%error, provider = %event.provider, "confluence bus publish failed");
                    "unavailable".to_string()
                })
        })
        .map_err(|_| BusError::Unavailable)?;
        if let Some(request_id) = outcome {
            // Duplicate: the guest sees the original acceptance. There is no
            // `duplicate` flag on `soil:unibus/receipt`, so a redelivery is
            // a plain 202 to the caller and one fact on the bus.
            return Ok(BusReceipt { request_id });
        }
        let seen = self.seen.lock().map_err(|_| BusError::Unavailable)?;
        seen.get(&key)
            .cloned()
            .map(|request_id| BusReceipt { request_id })
            .ok_or(BusError::Unavailable)
    }

    fn poll(
        &self,
        kinds: &[String],
        cursor: Option<&str>,
        limit: u32,
    ) -> std::result::Result<soil_wasi_host::Page, BusError> {
        // Observation is not Confluence policy: delegate so the same host can
        // serve observer Spores wired to the same inner bus.
        self.inner.poll(kinds, cursor, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockPublisher {
        events: Mutex<Vec<ConfluenceEvent>>,
        fail: bool,
    }

    impl Publisher for MockPublisher {
        fn publish(&self, event: &ConfluenceEvent) -> anyhow::Result<String> {
            if self.fail {
                anyhow::bail!("unibus down");
            }
            self.events.lock().unwrap().push(event.clone());
            Ok(format!("request-{}", self.events.lock().unwrap().len()))
        }
    }

    fn bus_body(id: &str) -> Vec<u8> {
        json!({
            "provider": "provider-a",
            "delivery_id": id,
            "event_type": "page_updated",
            "received_at_ms": 1_790_000_000_000u64,
            "headers": {"content-type": "application/json"},
            "body": {"action": "opened"},
            "tenant": "guest-value"
        })
        .to_string()
        .into_bytes()
    }

    fn event(id: &str) -> Event {
        Event {
            kind: EVENT_KIND.to_string(),
            body: json!({
                "provider": "provider-a",
                "delivery_id": id,
                "event_type": "page_updated",
                "received_at_ms": 1_790_000_000_000u64,
                "headers": {"content-type": "application/json"},
                "body": {"action": "opened"},
                "tenant": "guest-value"
            })
            .to_string()
            .into_bytes(),
            idempotency_key: Some(format!("provider-a:{id}")),
        }
    }

    #[test]
    fn stamps_the_host_tenant_and_deduplicates() {
        let publisher = Arc::new(MockPublisher::default());
        let sink = ConfluenceEventSink::new("tenant-a", publisher.clone());
        let first = sink.publish(&event("delivery-1")).unwrap();
        assert!(!first.duplicate);
        assert_eq!(first.request_id, "request-1");
        {
            let events = publisher.events.lock().unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].tenant, "tenant-a");
            assert_eq!(events[0].provider, "provider-a");
        }
        let second = sink.publish(&event("delivery-1")).unwrap();
        assert!(second.duplicate);
        assert_eq!(second.request_id, first.request_id);
        assert_eq!(publisher.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn confluence_bus_stamps_dedupes_and_delegates_poll() {
        use soil_wasi_host::MemoryBus;

        let inner = Arc::new(MemoryBus::default());
        let bus = ConfluenceBus::new("tenant-a", inner.clone());
        let first = bus
            .publish(EVENT_KIND, &bus_body("delivery-1"), None, 0)
            .unwrap();
        assert_eq!(first.request_id, "local-1");
        // Redelivery carries a different per-request `received_at_ms` in the
        // real Spore; the content key still dedupes.
        let mut redelivery: serde_json::Value =
            serde_json::from_slice(&bus_body("delivery-1")).unwrap();
        redelivery["received_at_ms"] = json!(1_790_000_001_000u64);
        let redelivery_bytes = serde_json::to_vec(&redelivery).unwrap();
        let second = bus.publish(EVENT_KIND, &redelivery_bytes, None, 0).unwrap();
        assert_eq!(second.request_id, first.request_id);
        assert_eq!(inner.frames().len(), 1);
        let stamped: serde_json::Value = serde_json::from_slice(&inner.frames()[0].body).unwrap();
        assert_eq!(stamped["tenant"], "tenant-a");

        let page = bus.poll(&[EVENT_KIND.to_string()], None, 10).unwrap();
        assert_eq!(page.frames.len(), 1);
        assert_eq!(page.frames[0].kind, EVENT_KIND);
    }

    #[test]
    fn confluence_bus_rejects_other_kinds_and_destinations() {
        use soil_wasi_host::MemoryBus;

        let bus = ConfluenceBus::new("tenant-a", Arc::new(MemoryBus::default()));
        assert_eq!(
            bus.publish("other.v1", &bus_body("d"), None, 0)
                .unwrap_err(),
            BusError::Denied
        );
        assert_eq!(
            bus.publish(EVENT_KIND, &bus_body("d"), Some("peer"), 0)
                .unwrap_err(),
            BusError::Invalid
        );
        assert_eq!(
            bus.publish(EVENT_KIND, br#"{"provider":"a"}"#, None, 0)
                .unwrap_err(),
            BusError::Invalid
        );
    }

    #[test]
    fn rejects_other_kinds_missing_fields_and_publish_failures() {
        let publisher = Arc::new(MockPublisher::default());
        let sink = ConfluenceEventSink::new("tenant-a", publisher);

        let mut wrong_kind = event("delivery-1");
        wrong_kind.kind = "confluence.other.v1".into();
        assert_eq!(sink.publish(&wrong_kind).unwrap_err(), EventError::Denied);

        let mut missing = event("delivery-1");
        missing.body = br#"{"provider":"provider-a"}"#.to_vec();
        assert_eq!(sink.publish(&missing).unwrap_err(), EventError::Invalid);

        let failing = ConfluenceEventSink::new(
            "tenant-a",
            Arc::new(MockPublisher {
                fail: true,
                ..Default::default()
            }),
        );
        assert_eq!(
            failing.publish(&event("delivery-2")).unwrap_err(),
            EventError::Unavailable
        );
    }
}
