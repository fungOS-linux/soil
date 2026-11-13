//! Confluence facts on the Unibus.
//!
//! The ingress is a Spore on the `wasi:http` surface; this crate holds the
//! event shape consumers depend on and the host-side publication policy in
//! [`spore`]. There is no native HTTP webhook path: the Spore is the only
//! ingress, so a signature rule cannot drift between two implementations.
//!
//! The Unibus names are the stable contract. Elmers and other consumers do not
//! depend on the process that published them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod admin;
pub mod hook_action;
pub mod spore;

pub const EVENT_KIND: &str = "confluence.webhook.received.v1";
/// One MiB, not 64 KiB: a real GitHub advisory record can exceed 100 KB
/// (crates.io advisories have reached 146 KB), and the ingress dropping a
/// security delivery for size is the one failure it cannot report.
pub const MAX_BODY: usize = 1024 * 1024;

/// The durable fact published to Unibus. Consumers must treat this as
/// at-least-once and deduplicate on `provider` + `delivery_id`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfluenceEvent {
    pub tenant: String,
    pub provider: String,
    /// Which signature scheme accepted the delivery (`github`, `gitea`,
    /// `standard-webhooks`). Provenance, not policy: a consumer that needs to
    /// know how the hook was authenticated reads it here instead of guessing
    /// from the provider name.
    #[serde(default)]
    pub scheme: String,
    pub delivery_id: String,
    pub event_type: String,
    pub received_at_ms: u64,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

pub trait Publisher: Send + Sync {
    fn publish(&self, event: &ConfluenceEvent) -> anyhow::Result<String>;
}

pub mod webhooks;
