//! Host-owned, blocking consent broker.
//!
//! Consent is not a second ACL and it is not a WIT-only Unibus feature.
//! Unibus, Spore Store, outbound actions and Spores all create the same
//! [`ConsentRequest`]; the host blocks the caller, a system consent surface
//! resolves it, and the caller then mints an ordinary Mycelium/Unibus/Soil
//! grant or refuses.
//!
//! The broker deliberately does not sign, persist or enforce grants. It owns
//! only pending requests, decisions and a small bounded audit history.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Condvar, Mutex},
    time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod client;
pub mod wire;

const MAX_HISTORY: usize = 256;
const MAX_TIMEOUT_MS: u64 = 300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Install,
    Capability,
    UnibusGrant,
    Outbound,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Write,
    External,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Once,
    Session,
    Persistent,
    Until(u64),
}

/// The preview is untrusted presentation data. The host must frame it with its
/// own trusted header, canonical facts and decision controls. Approval is bound
/// to the request's payload digest, never to what the preview rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Preview {
    None,
    /// A Spore-owned Canvas scene rendered inside a host-owned consent window.
    SporeScene {
        spore: String,
        view: String,
        summary: String,
    },
    /// Host/action-adapter artifact such as an email or chat message.
    Artifact {
        media_type: String,
        summary: String,
        digest: String,
    },
    /// Host-rendered form for explicit, schema-described asks.
    Schema {
        schema: String,
        json: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub kind: Kind,
    pub requester: String,
    /// Host-attested principal. Never caller-supplied.
    pub principal: String,
    /// Exact action/capability/route being approved.
    pub action: String,
    /// Digest of the canonical payload the decision authorizes.
    pub payload_digest: String,
    pub effects: Vec<Effect>,
    pub scopes: Vec<String>,
    pub resources: Vec<String>,
    pub requested_scope: Scope,
    pub preview: Preview,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub request_id: String,
    pub allowed: bool,
    pub scope: Scope,
    pub reason: Option<String>,
    pub decided_by: String,
    pub decided_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub request: Request,
    pub created_at_ms: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid consent request: {0}")]
    Invalid(&'static str),
    #[error("consent request {0} already exists")]
    Duplicate(String),
    #[error("consent request {0} not found")]
    NotFound(String),
    #[error("consent request {0} already decided")]
    AlreadyDecided(String),
    #[error("consent request {0} timed out")]
    Timeout(String),
}

struct Entry {
    request: Request,
    created_at_ms: u64,
    decision: Option<Decision>,
}

#[derive(Default)]
struct State {
    pending: BTreeMap<String, Entry>,
    history: VecDeque<(Request, Decision)>,
}

pub struct Broker {
    state: Mutex<State>,
    changed: Condvar,
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

impl Broker {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        }
    }

    /// Block until a decision arrives, the request expires, or the broker is
    /// dropped. No UI or no decision therefore means deny.
    pub fn request(&self, request: Request) -> Result<Decision, Error> {
        validate(&request)?;
        let request_id = request.id.clone();
        let timeout = StdDuration::from_millis(request.timeout_ms);
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().expect("consent broker poisoned");
        if state.pending.contains_key(&request_id) {
            return Err(Error::Duplicate(request_id));
        }
        state.pending.insert(
            request_id.clone(),
            Entry {
                request,
                created_at_ms: now_ms(),
                decision: None,
            },
        );

        loop {
            let Some(entry) = state.pending.get(&request_id) else {
                return Err(Error::NotFound(request_id));
            };
            if let Some(decision) = &entry.decision {
                let decision = decision.clone();
                if let Some(entry) = state.pending.remove(&request_id) {
                    state.history.push_back((entry.request, decision.clone()));
                    while state.history.len() > MAX_HISTORY {
                        state.history.pop_front();
                    }
                }
                self.changed.notify_all();
                return Ok(decision);
            }
            let now = Instant::now();
            if now >= deadline {
                state.pending.remove(&request_id);
                self.changed.notify_all();
                return Err(Error::Timeout(request_id));
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("consent broker poisoned");
            state = next;
        }
    }

    /// Records the user's decision. The waiting caller is released.
    pub fn resolve(&self, decision: Decision) -> Result<(), Error> {
        if decision.request_id.is_empty() || decision.decided_by.is_empty() {
            return Err(Error::Invalid("decision identity"));
        }
        let mut state = self.state.lock().expect("consent broker poisoned");
        let Some(entry) = state.pending.get_mut(&decision.request_id) else {
            return Err(Error::NotFound(decision.request_id));
        };
        if entry.decision.is_some() {
            return Err(Error::AlreadyDecided(decision.request_id));
        }
        entry.decision = Some(decision);
        self.changed.notify_all();
        Ok(())
    }

    /// Explicit cancellation is a deny, not a silent timeout.
    pub fn cancel(&self, request_id: &str, decided_by: impl Into<String>) -> Result<(), Error> {
        self.resolve(Decision {
            request_id: request_id.to_owned(),
            allowed: false,
            scope: Scope::Once,
            reason: Some("cancelled".into()),
            decided_by: decided_by.into(),
            decided_at_ms: now_ms(),
        })
    }

    pub fn pending(&self) -> Vec<Pending> {
        let state = self.state.lock().expect("consent broker poisoned");
        state
            .pending
            .values()
            .map(|entry| Pending {
                request: entry.request.clone(),
                created_at_ms: entry.created_at_ms,
            })
            .collect()
    }

    pub fn history(&self) -> Vec<(Request, Decision)> {
        let state = self.state.lock().expect("consent broker poisoned");
        state.history.iter().cloned().collect()
    }
}

fn validate(request: &Request) -> Result<(), Error> {
    if request.id.is_empty() || request.id.len() > 128 {
        return Err(Error::Invalid("id"));
    }
    if request.requester.is_empty()
        || request.action.is_empty()
        || request.payload_digest.is_empty()
    {
        return Err(Error::Invalid("identity/action/digest"));
    }
    if request.timeout_ms == 0 || request.timeout_ms > MAX_TIMEOUT_MS {
        return Err(Error::Invalid("timeout"));
    }
    if request.effects.is_empty() {
        return Err(Error::Invalid("effects"));
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, timeout_ms: u64) -> Request {
        Request {
            id: id.into(),
            kind: Kind::Outbound,
            requester: "spore:mail".into(),
            principal: "oidc:example#avery".into(),
            action: "mail.send".into(),
            payload_digest: "sha256:abc".into(),
            effects: vec![Effect::External],
            scopes: vec!["mail.send".into()],
            resources: vec!["3 recipients".into()],
            requested_scope: Scope::Once,
            preview: Preview::Artifact {
                media_type: "message/rfc822".into(),
                summary: "Hello".into(),
                digest: "sha256:def".into(),
            },
            timeout_ms,
        }
    }

    #[test]
    fn blocks_until_allow_and_consumes_the_request() {
        let broker = std::sync::Arc::new(Broker::new());
        let waiting = {
            let broker = broker.clone();
            std::thread::spawn(move || broker.request(request("req-1", 1_000)))
        };
        while broker.pending().is_empty() {
            std::thread::yield_now();
        }
        broker
            .resolve(Decision {
                request_id: "req-1".into(),
                allowed: true,
                scope: Scope::Once,
                reason: None,
                decided_by: "human:avery".into(),
                decided_at_ms: now_ms(),
            })
            .unwrap();
        let decision = waiting.join().unwrap().unwrap();
        assert!(decision.allowed);
        assert_eq!(decision.scope, Scope::Once);
        assert!(broker.pending().is_empty());
        assert_eq!(broker.history().len(), 1);
    }

    #[test]
    fn no_decision_times_out_and_denies() {
        let broker = Broker::new();
        assert_eq!(
            broker.request(request("req-2", 20)),
            Err(Error::Timeout("req-2".into()))
        );
        assert!(broker.pending().is_empty());
    }

    #[test]
    fn cancellation_is_a_denial() {
        let broker = std::sync::Arc::new(Broker::new());
        let waiting = {
            let broker = broker.clone();
            std::thread::spawn(move || broker.request(request("req-3", 1_000)))
        };
        while broker.pending().is_empty() {
            std::thread::yield_now();
        }
        broker.cancel("req-3", "human:avery").unwrap();
        let decision = waiting.join().unwrap().unwrap();
        assert!(!decision.allowed);
        assert_eq!(decision.reason.as_deref(), Some("cancelled"));
    }

    #[test]
    fn rejects_duplicate_ids_and_empty_effects() {
        let broker = std::sync::Arc::new(Broker::new());
        let waiting = {
            let broker = broker.clone();
            std::thread::spawn(move || broker.request(request("req-4", 1_000)))
        };
        while broker.pending().is_empty() {
            std::thread::yield_now();
        }
        assert_eq!(
            broker.request(request("req-4", 1_000)),
            Err(Error::Duplicate("req-4".into()))
        );
        broker.cancel("req-4", "test").unwrap();
        waiting.join().unwrap().unwrap();

        let mut invalid = request("req-5", 1_000);
        invalid.effects.clear();
        assert_eq!(broker.request(invalid), Err(Error::Invalid("effects")));
    }
}
