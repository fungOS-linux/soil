//! Host-owned action bus for Spores.
//!
//! A Spore names an action id, version, JSON input and an optional idempotency
//! key. This crate enforces the local grant, approval class, idempotency and
//! routing boundary. Unibus adapters implement [`ActionRouter`]; the Spore never
//! sees an endpoint, token, retry policy or connector library.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Write,
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    Idempotent,
    Keyed,
    Unsafe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    None,
    Operator,
    Destructive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDescriptor {
    pub id: String,
    pub version: u32,
    pub summary: String,
    pub effect: Effect,
    pub idempotency: Idempotency,
    pub approval: Approval,
    pub input_schema: String,
    pub output_schema: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionCall {
    pub action: String,
    pub version: u32,
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub action: String,
    pub version: u32,
    pub output: String,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    Denied,
    UnknownAction,
    UnsupportedVersion,
    Invalid(String),
    ApprovalRequired(Approval),
    UnsafeRetry,
    OverBudget,
    Unavailable(String),
    Io(String),
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied => f.write_str("action not granted"),
            Self::UnknownAction => f.write_str("unknown action"),
            Self::UnsupportedVersion => f.write_str("unsupported action version"),
            Self::Invalid(message) => write!(f, "invalid action call: {message}"),
            Self::ApprovalRequired(approval) => write!(f, "approval required: {approval:?}"),
            Self::UnsafeRetry => f.write_str("unsafe action cannot be retried automatically"),
            Self::OverBudget => f.write_str("action budget exceeded"),
            Self::Unavailable(message) => write!(f, "action unavailable: {message}"),
            Self::Io(message) => write!(f, "action host: {message}"),
        }
    }
}

impl std::error::Error for ActionError {}

/// The Unibus-facing adapter boundary. The adapter owns the connector and
/// credentials; it receives only an already-authorized call.
pub trait ActionRouter: Send + Sync {
    fn invoke(
        &self,
        descriptor: &ActionDescriptor,
        call: &ActionCall,
    ) -> Result<String, ActionError>;
}

/// Exact `(action id, version)` grants. A grant never implies another version.
#[derive(Clone, Debug, Default)]
pub struct ActionGrants {
    allowed: BTreeSet<(String, u32)>,
}

impl ActionGrants {
    pub fn allow(&mut self, id: impl Into<String>, version: u32) {
        self.allowed.insert((id.into(), version));
    }

    pub fn allows(&self, id: &str, version: u32) -> bool {
        self.allowed.contains(&(id.to_owned(), version))
    }
}

/// Durable idempotency receipts. Keyed actions replay the stored output instead
/// of invoking the adapter twice.
pub struct IdempotencyStore {
    db: Connection,
    lock: Mutex<()>,
}

impl IdempotencyStore {
    pub fn open(path: &Path) -> Result<Self, ActionError> {
        let db = Connection::open(path)
            .map_err(|error| ActionError::Io(format!("open idempotency store: {error}")))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS action_receipts(
               action_id TEXT NOT NULL,
               version INTEGER NOT NULL,
               idempotency_key TEXT NOT NULL,
               output TEXT NOT NULL,
               created_hlc INTEGER NOT NULL,
               PRIMARY KEY(action_id, version, idempotency_key)
             );",
        )
        .map_err(|error| ActionError::Io(format!("initialize idempotency store: {error}")))?;
        Ok(Self {
            db,
            lock: Mutex::new(()),
        })
    }

    pub fn in_memory() -> Result<Self, ActionError> {
        let db = Connection::open_in_memory()
            .map_err(|error| ActionError::Io(format!("open idempotency store: {error}")))?;
        db.execute_batch(
            "CREATE TABLE action_receipts(
               action_id TEXT NOT NULL,
               version INTEGER NOT NULL,
               idempotency_key TEXT NOT NULL,
               output TEXT NOT NULL,
               created_hlc INTEGER NOT NULL,
               PRIMARY KEY(action_id, version, idempotency_key)
             );",
        )
        .map_err(|error| ActionError::Io(format!("initialize idempotency store: {error}")))?;
        Ok(Self {
            db,
            lock: Mutex::new(()),
        })
    }

    fn get(&self, action: &str, version: u32, key: &str) -> Result<Option<String>, ActionError> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| ActionError::Io("idempotency lock poisoned".into()))?;
        self.db
            .query_row(
                "SELECT output FROM action_receipts
                 WHERE action_id=?1 AND version=?2 AND idempotency_key=?3",
                params![action, version as i64, key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| ActionError::Io(format!("read idempotency receipt: {error}")))
    }

    fn put(&self, action: &str, version: u32, key: &str, output: &str) -> Result<(), ActionError> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| ActionError::Io("idempotency lock poisoned".into()))?;
        self.db
            .execute(
                "INSERT OR REPLACE INTO action_receipts
                 (action_id,version,idempotency_key,output,created_hlc) VALUES(?1,?2,?3,?4,?5)",
                params![action, version as i64, key, output, now_ms() as i64],
            )
            .map_err(|error| ActionError::Io(format!("write idempotency receipt: {error}")))?;
        Ok(())
    }
}

pub struct ActionHost {
    catalog: BTreeMap<(String, u32), ActionDescriptor>,
    grants: ActionGrants,
    router: Arc<dyn ActionRouter>,
    idempotency: IdempotencyStore,
    allow_unsafe: bool,
}

impl ActionHost {
    pub fn new(
        catalog: impl IntoIterator<Item = ActionDescriptor>,
        grants: ActionGrants,
        router: Arc<dyn ActionRouter>,
        idempotency: IdempotencyStore,
    ) -> Result<Self, ActionError> {
        let mut by_key = BTreeMap::new();
        for descriptor in catalog {
            validate_descriptor(&descriptor)?;
            let key = (descriptor.id.clone(), descriptor.version);
            if by_key.insert(key, descriptor).is_some() {
                return Err(ActionError::Invalid("duplicate action descriptor".into()));
            }
        }
        Ok(Self {
            catalog: by_key,
            grants,
            router,
            idempotency,
            allow_unsafe: false,
        })
    }

    /// Unsafe actions are refused by default. A host may explicitly opt in for
    /// an operator-driven, non-retrying run.
    pub fn allow_unsafe(mut self, allow: bool) -> Self {
        self.allow_unsafe = allow;
        self
    }

    /// The catalog is already filtered by the local exact-version grants.
    pub fn catalog(&self) -> Vec<ActionDescriptor> {
        self.catalog
            .values()
            .filter(|descriptor| self.grants.allows(&descriptor.id, descriptor.version))
            .cloned()
            .collect()
    }

    pub fn invoke(&self, call: &ActionCall) -> Result<ActionReceipt, ActionError> {
        if call.action.len() > 128
            || call.action.trim().is_empty()
            || call.input.len() > 1024 * 1024
            || call
                .idempotency_key
                .as_deref()
                .is_some_and(|key| key.is_empty() || key.len() > 256)
        {
            return Err(ActionError::Invalid("call bounds".into()));
        }
        if !self.grants.allows(&call.action, call.version) {
            return Err(ActionError::Denied);
        }
        let descriptor = self
            .catalog
            .get(&(call.action.clone(), call.version))
            .ok_or(ActionError::UnknownAction)?;
        if descriptor.approval != Approval::None {
            return Err(ActionError::ApprovalRequired(descriptor.approval));
        }
        if descriptor.idempotency == Idempotency::Unsafe && !self.allow_unsafe {
            return Err(ActionError::UnsafeRetry);
        }

        let key = match descriptor.idempotency {
            Idempotency::Idempotent => None,
            Idempotency::Keyed | Idempotency::Unsafe => {
                let key = call
                    .idempotency_key
                    .as_deref()
                    .ok_or_else(|| ActionError::Invalid("idempotency key required".into()))?;
                Some(key)
            }
        };

        if let Some(key) = key {
            if let Some(output) = self.idempotency.get(&call.action, call.version, key)? {
                return Ok(ActionReceipt {
                    action: call.action.clone(),
                    version: call.version,
                    output,
                    replayed: true,
                });
            }
        }

        let output = self.router.invoke(descriptor, call)?;
        if let Some(key) = key {
            self.idempotency
                .put(&call.action, call.version, key, &output)?;
        }
        Ok(ActionReceipt {
            action: call.action.clone(),
            version: call.version,
            output,
            replayed: false,
        })
    }
}

fn validate_descriptor(descriptor: &ActionDescriptor) -> Result<(), ActionError> {
    if descriptor.id.trim().is_empty()
        || descriptor.id.len() > 128
        || descriptor.version == 0
        || descriptor.summary.trim().is_empty()
        || descriptor.summary.len() > 512
        || descriptor.input_schema.len() > 64 * 1024
        || descriptor.output_schema.len() > 64 * 1024
    {
        return Err(ActionError::Invalid("descriptor bounds".into()));
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeRouter {
        calls: AtomicUsize,
    }

    impl ActionRouter for FakeRouter {
        fn invoke(
            &self,
            descriptor: &ActionDescriptor,
            call: &ActionCall,
        ) -> Result<String, ActionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(format!("{}:{}", descriptor.id, call.input))
        }
    }

    fn descriptor(id: &str, idempotency: Idempotency, approval: Approval) -> ActionDescriptor {
        ActionDescriptor {
            id: id.into(),
            version: 1,
            summary: "test action".into(),
            effect: Effect::Write,
            idempotency,
            approval,
            input_schema: "{}".into(),
            output_schema: "{}".into(),
        }
    }

    fn test_host() -> (ActionHost, Arc<FakeRouter>) {
        let router = Arc::new(FakeRouter {
            calls: AtomicUsize::new(0),
        });
        let mut grants = ActionGrants::default();
        grants.allow("unibus.mail.send", 1);
        grants.allow("unibus.mail.read", 1);
        grants.allow("unibus.beeper.notify", 1);
        let host = ActionHost::new(
            [
                descriptor("unibus.mail.send", Idempotency::Keyed, Approval::None),
                descriptor("unibus.mail.read", Idempotency::Idempotent, Approval::None),
                descriptor("unibus.beeper.notify", Idempotency::Unsafe, Approval::None),
            ],
            grants,
            router.clone(),
            IdempotencyStore::in_memory().unwrap(),
        )
        .unwrap();
        (host, router)
    }

    #[test]
    fn catalog_is_filtered_by_exact_version_grant() {
        let (host, _) = test_host();
        assert_eq!(host.catalog().len(), 3);
        let denied = host
            .invoke(&ActionCall {
                action: "unibus.mail.send".into(),
                version: 2,
                input: "{}".into(),
                idempotency_key: Some("k".into()),
            })
            .unwrap_err();
        assert_eq!(denied, ActionError::Denied);
    }

    #[test]
    fn keyed_invocation_replays_instead_of_calling_twice() {
        let (host, router) = test_host();
        let call = ActionCall {
            action: "unibus.mail.send".into(),
            version: 1,
            input: r#"{"to":"a@example.com"}"#.into(),
            idempotency_key: Some("mail-1".into()),
        };
        let first = host.invoke(&call).unwrap();
        assert!(!first.replayed);
        let second = host.invoke(&call).unwrap();
        assert!(second.replayed);
        assert_eq!(first.output, second.output);
        assert_eq!(router.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn keyed_action_requires_a_key_and_unsafe_is_denied_by_default() {
        let (host, _) = test_host();
        assert!(matches!(
            host.invoke(&ActionCall {
                action: "unibus.mail.send".into(),
                version: 1,
                input: "{}".into(),
                idempotency_key: None,
            }),
            Err(ActionError::Invalid(_))
        ));
        assert_eq!(
            host.invoke(&ActionCall {
                action: "unibus.beeper.notify".into(),
                version: 1,
                input: "{}".into(),
                idempotency_key: Some("page-1".into()),
            })
            .unwrap_err(),
            ActionError::UnsafeRetry
        );
    }

    #[test]
    fn approval_and_denial_fail_before_the_router() {
        let router = Arc::new(FakeRouter {
            calls: AtomicUsize::new(0),
        });
        let mut grants = ActionGrants::default();
        grants.allow("unibus.destructive", 1);
        let host = ActionHost::new(
            [descriptor(
                "unibus.destructive",
                Idempotency::Idempotent,
                Approval::Operator,
            )],
            grants,
            router.clone(),
            IdempotencyStore::in_memory().unwrap(),
        )
        .unwrap();
        assert_eq!(
            host.invoke(&ActionCall {
                action: "unibus.destructive".into(),
                version: 1,
                input: "{}".into(),
                idempotency_key: None,
            })
            .unwrap_err(),
            ActionError::ApprovalRequired(Approval::Operator)
        );
        assert_eq!(router.calls.load(Ordering::SeqCst), 0);
    }
}
