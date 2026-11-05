//! Host-owned notification and ringer broker.
//!
//! This is deliberately a host crate, not a Spore: it owns app identity,
//! per-device inbox state, focus/quiet-hours policy, history retention, cleanup
//! receipts and the one-device ringer claim. The notify Spore is a presentation
//! client. Cross-device messages travel on the host-owned `unibus/peer/1`
//! session; Spores never import Unibus or Iroh.

use std::{
    collections::BTreeSet,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};

mod component;
mod peer;

pub use component::{NotifyComponent, NotifyRuntime, add_to_linker};
pub use peer::NotifyPeerAdapter;
use serde::{Deserialize, Serialize};

pub const NOTIFY_PUBLISH: &str = "notify.publish.v1";
pub const NOTIFY_UPDATE: &str = "notify.update.v1";
pub const NOTIFY_WITHDRAW: &str = "notify.withdraw.v1";
pub const NOTIFY_ACK: &str = "notify.ack.v1";
pub const NOTIFY_STATE: &str = "notify.state.v1";
pub const NOTIFY_CLEANUP: &str = "notify.cleanup.v1";
pub const NOTIFY_CLEANUP_UNDO: &str = "notify.cleanup-undo.v1";
pub const RINGER_INCOMING: &str = "ringer.incoming.v1";
pub const RINGER_ACCEPT: &str = "ringer.accept.v1";
pub const RINGER_DECLINE: &str = "ringer.decline.v1";
pub const RINGER_END: &str = "ringer.end.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Passive,
    Active,
    Urgent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub destructive: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency_hint: Option<Priority>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub app_id: String,
    pub source_device: String,
    pub title: String,
    pub body: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    pub priority: Priority,
    pub sensitive: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub envelope: Envelope,
    pub read: bool,
    pub held: bool,
    pub dismissed_at_ms: Option<u64>,
    pub snoozed_until_ms: Option<u64>,
    pub acted_action: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub apps: Vec<String>,
    pub categories: Vec<String>,
    pub include_held: bool,
    pub include_read: bool,
    pub offset: u64,
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusProfile {
    pub mode: String,
    pub until_ms: Option<u64>,
    pub allow_apps: Vec<String>,
    pub allow_categories: Vec<String>,
    pub allow_threads: Vec<String>,
    pub breakthrough: Vec<String>,
    pub rollup_held: bool,
}

impl FocusProfile {
    fn is_active(&self, now_ms: u64) -> bool {
        self.mode != "off" && self.until_ms.is_none_or(|until| until > now_ms)
    }

    fn holds(&self, envelope: &Envelope, now_ms: u64) -> bool {
        if !self.is_active(now_ms) {
            return false;
        }
        if self.breakthrough.iter().any(|key| {
            key == &envelope.category
                || key == &envelope.app_id
                || envelope.thread.as_ref() == Some(key)
        }) {
            return false;
        }
        let allowed = self.allow_apps.iter().any(|key| key == &envelope.app_id)
            || self
                .allow_categories
                .iter()
                .any(|key| key == &envelope.category)
            || envelope
                .thread
                .as_ref()
                .is_some_and(|thread| self.allow_threads.iter().any(|key| key == thread));
        !allowed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupScope {
    Read,
    Shown,
    Held,
    All,
    OlderThan,
    App,
    Category,
    Archive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReceipt {
    pub id: String,
    pub cleared: u32,
    pub undo_until_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingKind {
    Call,
    Doorbell,
    Intercom,
    Alarm,
    Timer,
    Pager,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingPhase {
    Ringing,
    Accepted,
    Declined,
    Ended,
    Missed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ring {
    pub id: String,
    pub kind: RingKind,
    pub source: String,
    pub title: String,
    pub priority: Priority,
    pub created_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RingState {
    pub id: String,
    pub phase: RingPhase,
    pub answered_by: Option<String>,
    pub muted_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRow {
    pub id: i64,
    pub peer: String,
    pub kind: String,
    pub payload: String,
    pub attempts: u32,
    pub next_attempt_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifyError {
    Denied,
    NotFound,
    NotRinging,
    AlreadyAnswered,
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied => f.write_str("notification capability denied"),
            Self::NotFound => f.write_str("notification not found"),
            Self::NotRinging => f.write_str("ring is not active"),
            Self::AlreadyAnswered => f.write_str("ring was already answered"),
            Self::Invalid(message) => write!(f, "invalid notification: {message}"),
            Self::Io(message) => write!(f, "notification host: {message}"),
        }
    }
}

impl std::error::Error for NotifyError {}

impl From<rusqlite::Error> for NotifyError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Host transport seam. A real implementation routes these kinds over the
/// shared `unibus/peer/1` session; no Spore sees this trait.
pub trait PeerRouter: Send + Sync {
    fn deliver(&self, peer: &str, kind: &str, payload: &str) -> Result<(), String>;
}

pub struct NotifyBroker {
    db: Mutex<Connection>,
    device_id: String,
    history_enabled: bool,
    undo_window_ms: u64,
    peers: Vec<String>,
}

impl NotifyBroker {
    pub fn open(path: &Path, device_id: impl Into<String>) -> Result<Self, NotifyError> {
        let db = Connection::open(path)?;
        Self::init(db, device_id)
    }

    pub fn in_memory(device_id: impl Into<String>) -> Result<Self, NotifyError> {
        Self::init(Connection::open_in_memory()?, device_id)
    }

    fn init(db: Connection, device_id: impl Into<String>) -> Result<Self, NotifyError> {
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS envelopes(
               id TEXT PRIMARY KEY,
               app_id TEXT NOT NULL,
               source_device TEXT NOT NULL,
               title TEXT NOT NULL,
               body TEXT NOT NULL,
               category TEXT NOT NULL,
               thread TEXT,
               group_key TEXT,
               created_at_ms INTEGER NOT NULL,
               expires_at_ms INTEGER,
               priority TEXT NOT NULL,
               sensitive INTEGER NOT NULL,
               actions_json TEXT NOT NULL,
               context_json TEXT
             );
             CREATE TABLE IF NOT EXISTS inbox_state(
               device_id TEXT NOT NULL,
               notification_id TEXT NOT NULL,
               read INTEGER NOT NULL DEFAULT 0,
               dismissed_at_ms INTEGER,
               snoozed_until_ms INTEGER,
               acted_action TEXT,
               PRIMARY KEY(device_id, notification_id)
             );
             CREATE TABLE IF NOT EXISTS focus(
               device_id TEXT PRIMARY KEY,
               mode TEXT NOT NULL,
               until_ms INTEGER,
               allow_apps TEXT NOT NULL,
               allow_categories TEXT NOT NULL,
               allow_threads TEXT NOT NULL,
               breakthrough TEXT NOT NULL,
               rollup_held INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS settings(
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS rings(
               id TEXT PRIMARY KEY,
               kind TEXT NOT NULL,
               source TEXT NOT NULL,
               title TEXT NOT NULL,
               priority TEXT NOT NULL,
               created_at_ms INTEGER NOT NULL,
               expires_at_ms INTEGER,
               actions_json TEXT NOT NULL,
               phase TEXT NOT NULL,
               answered_by TEXT,
               muted_by TEXT
             );
             CREATE TABLE IF NOT EXISTS outbox(
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               peer TEXT NOT NULL,
               kind TEXT NOT NULL,
               payload TEXT NOT NULL,
               attempts INTEGER NOT NULL DEFAULT 0,
               next_attempt_ms INTEGER NOT NULL DEFAULT 0,
               acked INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS cleanup_undo(
               id TEXT PRIMARY KEY,
               state_json TEXT NOT NULL,
               created_at_ms INTEGER NOT NULL
             );",
        )?;
        Ok(Self {
            db: Mutex::new(db),
            device_id: device_id.into(),
            history_enabled: false,
            undo_window_ms: 15_000,
            peers: Vec::new(),
        })
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Configure the peer fanout for durable outbox rows. An empty list uses
    /// the legacy `*` broadcast row; production hosts should set explicit peers.
    pub fn set_peers(&mut self, peers: impl IntoIterator<Item = String>) {
        self.peers = peers.into_iter().collect();
    }

    /// History is an explicit host/recipient setting, not implied by inbox.
    pub fn set_history_enabled(&mut self, enabled: bool) {
        self.history_enabled = enabled;
    }

    pub fn history_enabled(&self) -> bool {
        self.history_enabled
    }

    /// Publisher priority is advisory. A publisher can never self-promote to
    /// critical; urgent is clamped to active. Critical delivery needs a separate
    /// grant and recipient policy outside this method.
    pub fn publish(&self, mut envelope: Envelope) -> Result<String, NotifyError> {
        if envelope.title.is_empty() || envelope.category.is_empty() {
            return Err(NotifyError::Invalid(
                "title and category are required".into(),
            ));
        }
        if envelope.priority == Priority::Urgent {
            envelope.priority = Priority::Active;
        }
        if envelope.priority == Priority::Passive
            && envelope
                .context
                .as_ref()
                .is_some_and(|context| context.urgency_hint == Some(Priority::Urgent))
        {
            envelope.priority = Priority::Active;
        }
        let mut db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let tx = db.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO envelopes
             (id,app_id,source_device,title,body,category,thread,group_key,
              created_at_ms,expires_at_ms,priority,sensitive,actions_json,context_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                envelope.id,
                envelope.app_id,
                envelope.source_device,
                envelope.title,
                envelope.body,
                envelope.category,
                envelope.thread,
                envelope.group_key,
                envelope.created_at_ms as i64,
                envelope.expires_at_ms.map(|value| value as i64),
                priority_name(envelope.priority),
                envelope.sensitive as i64,
                serde_json::to_string(&envelope.actions).unwrap_or_else(|_| "[]".into()),
                envelope
                    .context
                    .as_ref()
                    .and_then(|context| serde_json::to_string(context).ok()),
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO inbox_state(device_id,notification_id)
             VALUES(?1,?2)",
            params![self.device_id, envelope.id],
        )?;
        enqueue(
            &tx,
            &self.peers,
            NOTIFY_PUBLISH,
            &serde_json::to_string(&envelope).unwrap_or_default(),
            now_ms(),
        )?;
        tx.commit()?;
        Ok(envelope.id)
    }

    /// Apply a remote envelope. `id` is the dedupe key; this is at-least-once.
    pub fn apply_remote(&self, envelope: Envelope) -> Result<bool, NotifyError> {
        let id = envelope.id.clone();
        let mut db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let tx = db.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO envelopes
             (id,app_id,source_device,title,body,category,thread,group_key,
              created_at_ms,expires_at_ms,priority,sensitive,actions_json,context_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                envelope.id,
                envelope.app_id,
                envelope.source_device,
                envelope.title,
                envelope.body,
                envelope.category,
                envelope.thread,
                envelope.group_key,
                envelope.created_at_ms as i64,
                envelope.expires_at_ms.map(|value| value as i64),
                priority_name(envelope.priority),
                envelope.sensitive as i64,
                serde_json::to_string(&envelope.actions).unwrap_or_else(|_| "[]".into()),
                envelope
                    .context
                    .as_ref()
                    .and_then(|context| serde_json::to_string(context).ok()),
            ],
        )? > 0;
        if inserted {
            tx.execute(
                "INSERT OR IGNORE INTO inbox_state(device_id,notification_id)
                 VALUES(?1,?2)",
                params![self.device_id, id],
            )?;
            enqueue_one(
                &tx,
                &envelope.source_device,
                NOTIFY_ACK,
                &serde_json::json!({ "id": id }).to_string(),
                now_ms(),
            )?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Update an envelope only when it still belongs to the same app.
    pub fn update(&self, id: &str, envelope: &Envelope) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let owner: Option<String> = db
            .query_row(
                "SELECT app_id FROM envelopes WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        match owner {
            None => return Err(NotifyError::NotFound),
            Some(owner) if owner != envelope.app_id => return Err(NotifyError::Denied),
            Some(_) => {}
        }
        db.execute(
            "UPDATE envelopes SET title=?1,body=?2,category=?3,thread=?4,group_key=?5,
             expires_at_ms=?6,priority=?7,sensitive=?8,actions_json=?9,context_json=?10
             WHERE id=?11",
            params![
                envelope.title,
                envelope.body,
                envelope.category,
                envelope.thread,
                envelope.group_key,
                envelope.expires_at_ms.map(|value| value as i64),
                priority_name(envelope.priority),
                envelope.sensitive as i64,
                serde_json::to_string(&envelope.actions).unwrap_or_else(|_| "[]".into()),
                envelope
                    .context
                    .as_ref()
                    .and_then(|context| serde_json::to_string(context).ok()),
                id,
            ],
        )?;
        enqueue(
            &db,
            &self.peers,
            NOTIFY_UPDATE,
            &serde_json::to_string(envelope).unwrap_or_default(),
            now_ms(),
        )?;
        Ok(())
    }

    /// Withdraw an envelope and its local inbox state.
    pub fn withdraw(&self, app_id: &str, id: &str) -> Result<(), NotifyError> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let tx = db.transaction()?;
        let owner: Option<String> = tx
            .query_row(
                "SELECT app_id FROM envelopes WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        match owner {
            None => return Err(NotifyError::NotFound),
            Some(owner) if owner != app_id => return Err(NotifyError::Denied),
            Some(_) => {}
        }
        tx.execute(
            "DELETE FROM inbox_state WHERE notification_id=?1",
            params![id],
        )?;
        tx.execute("DELETE FROM envelopes WHERE id=?1", params![id])?;
        enqueue(
            &tx,
            &self.peers,
            NOTIFY_WITHDRAW,
            &serde_json::json!({ "id": id, "app_id": app_id }).to_string(),
            now_ms(),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Apply a replicated read/dismiss state from another device.
    pub fn apply_remote_state(
        &self,
        id: &str,
        read: bool,
        dismissed: bool,
        acted_action: Option<&str>,
    ) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        db.execute(
            "UPDATE inbox_state SET read=?1,
             dismissed_at_ms=CASE WHEN ?2 THEN ?3 ELSE dismissed_at_ms END,
             acted_action=COALESCE(?4, acted_action)
             WHERE device_id=?5 AND notification_id=?6",
            params![
                read as i64,
                dismissed as i64,
                now_ms() as i64,
                acted_action,
                self.device_id,
                id,
            ],
        )?;
        Ok(())
    }

    /// Apply a remote withdraw without echoing it back onto the wire.
    pub fn apply_remote_withdraw(&self, id: &str) -> Result<(), NotifyError> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let tx = db.transaction()?;
        tx.execute(
            "DELETE FROM inbox_state WHERE notification_id=?1",
            params![id],
        )?;
        tx.execute("DELETE FROM envelopes WHERE id=?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// Apply a remote ring without re-broadcasting it.
    pub fn apply_remote_ring(&self, ring: &Ring) -> Result<bool, NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let inserted = db.execute(
            "INSERT OR IGNORE INTO rings
             (id,kind,source,title,priority,created_at_ms,expires_at_ms,actions_json,phase)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                ring.id,
                ring_kind_name(ring.kind),
                ring.source,
                ring.title,
                priority_name(ring.priority),
                ring.created_at_ms as i64,
                ring.expires_at_ms.map(|value| value as i64),
                serde_json::to_string(&ring.actions).unwrap_or_else(|_| "[]".into()),
                ring_phase_name(RingPhase::Ringing),
            ],
        )? > 0;
        Ok(inserted)
    }

    /// Apply a remote ring phase transition. The first accepted device wins.
    pub fn apply_remote_ring_phase(
        &self,
        id: &str,
        phase: RingPhase,
        answered_by: Option<&str>,
    ) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        db.execute(
            "UPDATE rings SET phase=?1,answered_by=?2
             WHERE id=?3 AND phase=?4",
            params![
                ring_phase_name(phase),
                answered_by,
                id,
                ring_phase_name(RingPhase::Ringing),
            ],
        )?;
        Ok(())
    }

    /// The first active ring, for a polled ringer subscription.
    pub fn active_ring(&self) -> Result<Option<RingState>, NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        db.query_row(
            "SELECT id,phase,answered_by,muted_by FROM rings
             WHERE phase=?1 ORDER BY created_at_ms ASC LIMIT 1",
            params![ring_phase_name(RingPhase::Ringing)],
            |row| {
                Ok(RingState {
                    id: row.get(0)?,
                    phase: parse_phase(&row.get::<_, String>(1)?),
                    answered_by: row.get(2)?,
                    muted_by: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn inbox(&self, filter: &Filter) -> Result<Vec<InboxItem>, NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let focus = self.read_focus(&db)?;
        let mut stmt = db.prepare(
            "SELECT e.id,e.app_id,e.source_device,e.title,e.body,e.category,e.thread,
                    e.group_key,e.created_at_ms,e.expires_at_ms,e.priority,e.sensitive,
                    e.actions_json,e.context_json,s.read,s.dismissed_at_ms,
                    s.snoozed_until_ms,s.acted_action
             FROM envelopes e JOIN inbox_state s ON s.notification_id=e.id
             WHERE s.device_id=?1
             ORDER BY e.created_at_ms DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(
            params![
                self.device_id,
                filter.limit.max(1) as i64,
                filter.offset as i64
            ],
            read_inbox_row,
        )?;
        let now = now_ms();
        let mut items = Vec::new();
        for row in rows {
            let item = row?;
            let envelope = &item.envelope;
            if envelope.expires_at_ms.is_some_and(|expires| expires <= now) {
                continue;
            }
            if !filter.apps.is_empty() && !filter.apps.iter().any(|app| app == &envelope.app_id) {
                continue;
            }
            if !filter.categories.is_empty()
                && !filter
                    .categories
                    .iter()
                    .any(|category| category == &envelope.category)
            {
                continue;
            }
            if !filter.include_read && item.read {
                continue;
            }
            let held = focus
                .as_ref()
                .is_some_and(|focus| focus.holds(envelope, now));
            if held && !filter.include_held {
                continue;
            }
            items.push(InboxItem { held, ..item });
        }
        Ok(items)
    }

    pub fn focus(&self) -> Result<Option<FocusProfile>, NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        self.read_focus(&db)
    }

    pub fn set_focus(&self, profile: FocusProfile) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        db.execute(
            "INSERT OR REPLACE INTO focus
             (device_id,mode,until_ms,allow_apps,allow_categories,allow_threads,
              breakthrough,rollup_held)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                self.device_id,
                profile.mode,
                profile.until_ms.map(|value| value as i64),
                serde_json::to_string(&profile.allow_apps).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&profile.allow_categories).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&profile.allow_threads).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&profile.breakthrough).unwrap_or_else(|_| "[]".into()),
                profile.rollup_held as i64,
            ],
        )?;
        Ok(())
    }

    pub fn clear_focus(&self) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        db.execute(
            "DELETE FROM focus WHERE device_id=?1",
            params![self.device_id],
        )?;
        Ok(())
    }

    pub fn dismiss(&self, id: &str) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let changed = db.execute(
            "UPDATE inbox_state SET read=1,dismissed_at_ms=?1
             WHERE device_id=?2 AND notification_id=?3",
            params![now_ms() as i64, self.device_id, id],
        )?;
        if changed == 0 {
            return Err(NotifyError::NotFound);
        }
        enqueue(
            &db,
            &self.peers,
            NOTIFY_STATE,
            &serde_json::json!({ "id": id, "device": self.device_id, "read": true, "dismissed": true, "acted_action": null })
                .to_string(),
            now_ms(),
        )?;
        Ok(())
    }

    pub fn act(&self, id: &str, action_id: &str) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let actions: String = db
            .query_row(
                "SELECT actions_json FROM envelopes WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(NotifyError::NotFound)?;
        let actions: Vec<Action> = serde_json::from_str(&actions).unwrap_or_default();
        if !actions.iter().any(|action| action.id == action_id) {
            return Err(NotifyError::Invalid("action is not registered".into()));
        }
        db.execute(
            "UPDATE inbox_state SET read=1,acted_action=?1
             WHERE device_id=?2 AND notification_id=?3",
            params![action_id, self.device_id, id],
        )?;
        enqueue(
            &db,
            &self.peers,
            NOTIFY_STATE,
            &serde_json::json!({
                "id": id,
                "device": self.device_id,
                "read": true,
                "dismissed": false,
                "acted_action": action_id,
            })
            .to_string(),
            now_ms(),
        )?;
        Ok(())
    }

    pub fn history(&self, offset: u64, limit: u32) -> Result<Vec<Envelope>, NotifyError> {
        if !self.history_enabled {
            return Err(NotifyError::Denied);
        }
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let mut stmt = db.prepare(
            "SELECT e.id,e.app_id,e.source_device,e.title,e.body,e.category,e.thread,
                    e.group_key,e.created_at_ms,e.expires_at_ms,e.priority,e.sensitive,
                    e.actions_json,e.context_json
             FROM envelopes e JOIN inbox_state s ON s.notification_id=e.id
             WHERE s.device_id=?1
             ORDER BY e.created_at_ms DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(
            params![self.device_id, limit.max(1) as i64, offset as i64],
            read_envelope_row,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn cleanup(
        &self,
        scope: CleanupScope,
        value: Option<&str>,
        older_than_ms: Option<u64>,
    ) -> Result<CleanupReceipt, NotifyError> {
        let id = format!("cleanup-{}:{}", self.device_id, now_ms());
        self.cleanup_inner(scope, value, older_than_ms, &id, true)
    }

    /// Apply a replicated cleanup without echoing it back.
    pub fn apply_remote_cleanup(
        &self,
        scope: CleanupScope,
        value: Option<&str>,
        older_than_ms: Option<u64>,
        receipt: &str,
    ) -> Result<u32, NotifyError> {
        self.cleanup_inner(scope, value, older_than_ms, receipt, false)
            .map(|receipt| receipt.cleared)
    }

    fn cleanup_inner(
        &self,
        scope: CleanupScope,
        value: Option<&str>,
        older_than_ms: Option<u64>,
        receipt_id: &str,
        broadcast: bool,
    ) -> Result<CleanupReceipt, NotifyError> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let focus = self.read_focus(&db)?;
        let now = now_ms();
        let mut stmt = db.prepare(
            "SELECT s.notification_id,s.read,s.dismissed_at_ms,s.snoozed_until_ms,
                    s.acted_action,e.app_id,e.category,e.created_at_ms
             FROM inbox_state s JOIN envelopes e ON e.id=s.notification_id
             WHERE s.device_id=?1",
        )?;
        let rows = stmt.query_map(params![self.device_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;
        let mut selected = Vec::new();
        for row in rows {
            let (id, read, dismissed, snoozed, acted, app, category, created) = row?;
            let envelope = Envelope {
                id: id.clone(),
                app_id: app.clone(),
                source_device: String::new(),
                title: String::new(),
                body: String::new(),
                category: category.clone(),
                thread: None,
                group_key: None,
                created_at_ms: created as u64,
                expires_at_ms: None,
                priority: Priority::Passive,
                sensitive: false,
                actions: Vec::new(),
                context: None,
            };
            let held = focus
                .as_ref()
                .is_some_and(|focus| focus.holds(&envelope, now));
            let keep = match scope {
                CleanupScope::Read => read,
                CleanupScope::Shown => true,
                CleanupScope::Held => held,
                CleanupScope::All => true,
                CleanupScope::OlderThan => {
                    older_than_ms.is_none_or(|age| now.saturating_sub(created as u64) >= age)
                }
                CleanupScope::App => value.is_some_and(|value| value == app),
                CleanupScope::Category => value.is_some_and(|value| value == category),
                CleanupScope::Archive => dismissed.is_some() || acted.is_some(),
            };
            if keep {
                selected.push(serde_json::json!({
                    "notification_id": id,
                    "read": read,
                    "dismissed_at_ms": dismissed,
                    "snoozed_until_ms": snoozed,
                    "acted_action": acted,
                }));
            }
        }
        drop(stmt);
        if selected.is_empty() {
            return Ok(CleanupReceipt {
                id: receipt_id.to_owned(),
                cleared: 0,
                undo_until_ms: now + self.undo_window_ms,
            });
        }
        let receipt = receipt_id.to_owned();
        let tx = db.transaction()?;
        for row in &selected {
            if let Some(id) = row.get("notification_id").and_then(|value| value.as_str()) {
                tx.execute(
                    "DELETE FROM inbox_state WHERE device_id=?1 AND notification_id=?2",
                    params![self.device_id, id],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO cleanup_undo(id,state_json,created_at_ms) VALUES(?1,?2,?3)",
            params![
                receipt,
                serde_json::to_string(&selected).unwrap_or_default(),
                now as i64
            ],
        )?;
        if broadcast {
            enqueue(
                &tx,
                &self.peers,
                NOTIFY_CLEANUP,
                &serde_json::json!({
                    "id": receipt,
                    "scope": cleanup_scope_name(scope),
                    "value": value,
                    "older_than_ms": older_than_ms,
                    "cleared": selected.len(),
                })
                .to_string(),
                now,
            )?;
        }
        tx.commit()?;
        Ok(CleanupReceipt {
            id: receipt,
            cleared: selected.len() as u32,
            undo_until_ms: now + self.undo_window_ms,
        })
    }

    pub fn undo_cleanup(&self, receipt: &str) -> Result<u32, NotifyError> {
        self.undo_cleanup_inner(receipt, true)
    }

    /// Apply a replicated cleanup undo without echoing it back.
    pub fn apply_remote_cleanup_undo(&self, receipt: &str) -> Result<u32, NotifyError> {
        self.undo_cleanup_inner(receipt, false)
    }

    fn undo_cleanup_inner(&self, receipt: &str, broadcast: bool) -> Result<u32, NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let row: Option<(String, i64)> = db
            .query_row(
                "SELECT state_json,created_at_ms FROM cleanup_undo WHERE id=?1",
                params![receipt],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state_json, created)) = row else {
            return Err(NotifyError::NotFound);
        };
        if now_ms().saturating_sub(created as u64) > self.undo_window_ms {
            db.execute("DELETE FROM cleanup_undo WHERE id=?1", params![receipt])?;
            return Err(NotifyError::Denied);
        }
        let rows: Vec<serde_json::Value> = serde_json::from_str(&state_json).unwrap_or_default();
        let mut restored = 0;
        for row in rows {
            if let Some(id) = row.get("notification_id").and_then(|value| value.as_str()) {
                db.execute(
                    "INSERT OR IGNORE INTO inbox_state
                     (device_id,notification_id,read,dismissed_at_ms,snoozed_until_ms,acted_action)
                     VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        self.device_id,
                        id,
                        row.get("read")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false) as i64,
                        row.get("dismissed_at_ms").and_then(|value| value.as_i64()),
                        row.get("snoozed_until_ms").and_then(|value| value.as_i64()),
                        row.get("acted_action").and_then(|value| value.as_str()),
                    ],
                )?;
                restored += 1;
            }
        }
        db.execute("DELETE FROM cleanup_undo WHERE id=?1", params![receipt])?;
        if broadcast {
            enqueue(
                &db,
                &self.peers,
                NOTIFY_CLEANUP_UNDO,
                &serde_json::json!({ "id": receipt }).to_string(),
                now_ms(),
            )?;
        }
        Ok(restored)
    }

    pub fn ring(&self, ring: Ring) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        db.execute(
            "INSERT INTO rings
             (id,kind,source,title,priority,created_at_ms,expires_at_ms,actions_json,phase)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                ring.id,
                ring_kind_name(ring.kind),
                ring.source,
                ring.title,
                priority_name(ring.priority),
                ring.created_at_ms as i64,
                ring.expires_at_ms.map(|value| value as i64),
                serde_json::to_string(&ring.actions).unwrap_or_else(|_| "[]".into()),
                ring_phase_name(RingPhase::Ringing),
            ],
        )?;
        enqueue(
            &db,
            &self.peers,
            RINGER_INCOMING,
            &serde_json::to_string(&ring).unwrap_or_default(),
            now_ms(),
        )?;
        Ok(())
    }

    pub fn ring_state(&self, id: &str) -> Result<RingState, NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        read_ring_state(&db, id)
    }

    pub fn accept_ring(&self, id: &str) -> Result<RingState, NotifyError> {
        self.transition_ring(id, RingPhase::Accepted)
    }

    pub fn decline_ring(&self, id: &str) -> Result<RingState, NotifyError> {
        self.transition_ring(id, RingPhase::Declined)
    }

    pub fn end_ring(&self, id: &str) -> Result<RingState, NotifyError> {
        self.transition_ring(id, RingPhase::Ended)
    }

    pub fn mute_ring(&self, id: &str) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let changed = db.execute(
            "UPDATE rings SET muted_by=?1 WHERE id=?2 AND phase=?3",
            params![self.device_id, id, ring_phase_name(RingPhase::Ringing)],
        )?;
        if changed == 0 {
            return Err(NotifyError::NotRinging);
        }
        Ok(())
    }

    fn transition_ring(&self, id: &str, phase: RingPhase) -> Result<RingState, NotifyError> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let tx = db.transaction()?;
        let current: (String, Option<String>) = tx
            .query_row(
                "SELECT phase,answered_by FROM rings WHERE id=?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(NotifyError::NotFound)?;
        let current_phase = parse_phase(&current.0);
        if current_phase != RingPhase::Ringing {
            return Err(if current.1.is_some() {
                NotifyError::AlreadyAnswered
            } else {
                NotifyError::NotRinging
            });
        }
        let answered_by = (phase == RingPhase::Accepted).then_some(self.device_id.clone());
        tx.execute(
            "UPDATE rings SET phase=?1,answered_by=?2 WHERE id=?3",
            params![ring_phase_name(phase), answered_by, id],
        )?;
        let kind = match phase {
            RingPhase::Accepted => RINGER_ACCEPT,
            RingPhase::Declined => RINGER_DECLINE,
            RingPhase::Ended | RingPhase::Missed => RINGER_END,
            RingPhase::Ringing => RINGER_INCOMING,
        };
        enqueue(
            &tx,
            &self.peers,
            kind,
            &serde_json::json!({
                "id": id,
                "device": self.device_id,
                "phase": ring_phase_name(phase),
            })
            .to_string(),
            now_ms(),
        )?;
        tx.commit()?;
        read_ring_state(&db, id)
    }

    /// Expire ringing rows. A missed ring becomes a normal deferred notification
    /// only after it is no longer active; the active ring is not history.
    pub fn expire_rings(&self, now_ms: u64) -> Result<u32, NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let mut stmt = db.prepare(
            "SELECT id FROM rings WHERE phase=?1 AND expires_at_ms IS NOT NULL AND expires_at_ms<=?2",
        )?;
        let ids = stmt
            .query_map(
                params![ring_phase_name(RingPhase::Ringing), now_ms as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for id in &ids {
            db.execute(
                "UPDATE rings SET phase=?1 WHERE id=?2",
                params![ring_phase_name(RingPhase::Missed), id],
            )?;
        }
        Ok(ids.len() as u32)
    }

    pub fn pending_outbox(&self, now_ms: u64, limit: u32) -> Result<Vec<OutboxRow>, NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let mut stmt = db.prepare(
            "SELECT id,peer,kind,payload,attempts,next_attempt_ms FROM outbox
             WHERE acked=0 AND next_attempt_ms<=?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now_ms as i64, limit.max(1) as i64], |row| {
            Ok(OutboxRow {
                id: row.get(0)?,
                peer: row.get(1)?,
                kind: row.get(2)?,
                payload: row.get(3)?,
                attempts: row.get::<_, i64>(4)? as u32,
                next_attempt_ms: row.get::<_, i64>(5)? as u64,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn flush_outbox(
        &self,
        router: &dyn PeerRouter,
        now_ms: u64,
        limit: u32,
    ) -> Result<u32, NotifyError> {
        let rows = self.pending_outbox(now_ms, limit)?;
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        let mut delivered = 0;
        for row in rows {
            match router.deliver(&row.peer, &row.kind, &row.payload) {
                Ok(()) => {
                    db.execute(
                        "UPDATE outbox SET acked=1,attempts=attempts+1 WHERE id=?1",
                        params![row.id],
                    )?;
                    delivered += 1;
                }
                Err(_) => {
                    let attempts = row.attempts + 1;
                    let backoff = 1_000u64.saturating_mul(1 << attempts.min(8));
                    db.execute(
                        "UPDATE outbox SET attempts=?1,next_attempt_ms=?2 WHERE id=?3",
                        params![attempts as i64, (now_ms + backoff) as i64, row.id],
                    )?;
                }
            }
        }
        Ok(delivered)
    }

    pub fn ack_outbox(&self, id: i64) -> Result<(), NotifyError> {
        let db = self
            .db
            .lock()
            .map_err(|_| NotifyError::Io("lock poisoned".into()))?;
        db.execute("UPDATE outbox SET acked=1 WHERE id=?1", params![id])?;
        Ok(())
    }

    fn read_focus(&self, db: &Connection) -> Result<Option<FocusProfile>, NotifyError> {
        db.query_row(
            "SELECT mode,until_ms,allow_apps,allow_categories,allow_threads,breakthrough,rollup_held
             FROM focus WHERE device_id=?1",
            params![self.device_id],
            |row| {
                Ok(FocusProfile {
                    mode: row.get(0)?,
                    until_ms: row.get::<_, Option<i64>>(1)?.map(|value| value as u64),
                    allow_apps: serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default(),
                    allow_categories: serde_json::from_str(&row.get::<_, String>(3)?)
                        .unwrap_or_default(),
                    allow_threads: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    breakthrough: serde_json::from_str(&row.get::<_, String>(5)?)
                        .unwrap_or_default(),
                    rollup_held: row.get::<_, i64>(6)? != 0,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }
}

fn enqueue(
    db: &Connection,
    peers: &[String],
    kind: &str,
    payload: &str,
    now_ms: u64,
) -> rusqlite::Result<()> {
    if peers.is_empty() {
        return enqueue_one(db, "*", kind, payload, now_ms);
    }
    for peer in peers {
        enqueue_one(db, peer, kind, payload, now_ms)?;
    }
    Ok(())
}

fn enqueue_one(
    db: &Connection,
    peer: &str,
    kind: &str,
    payload: &str,
    now_ms: u64,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO outbox(peer,kind,payload,next_attempt_ms)
         VALUES(?1,?2,?3,?4)",
        params![peer, kind, payload, now_ms as i64],
    )?;
    Ok(())
}

fn read_inbox_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InboxItem> {
    Ok(InboxItem {
        envelope: read_envelope_row(row)?,
        read: row.get(14)?,
        held: false,
        dismissed_at_ms: row.get::<_, Option<i64>>(15)?.map(|value| value as u64),
        snoozed_until_ms: row.get::<_, Option<i64>>(16)?.map(|value| value as u64),
        acted_action: row.get(17)?,
    })
}

fn read_envelope_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Envelope> {
    let priority: String = row.get(10)?;
    Ok(Envelope {
        id: row.get(0)?,
        app_id: row.get(1)?,
        source_device: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        category: row.get(5)?,
        thread: row.get(6)?,
        group_key: row.get(7)?,
        created_at_ms: row.get::<_, i64>(8)? as u64,
        expires_at_ms: row.get::<_, Option<i64>>(9)?.map(|value| value as u64),
        priority: parse_priority(&priority),
        sensitive: row.get::<_, bool>(11)?,
        actions: serde_json::from_str(&row.get::<_, String>(12)?).unwrap_or_default(),
        context: row
            .get::<_, Option<String>>(13)?
            .and_then(|value| serde_json::from_str(&value).ok()),
    })
}

fn read_ring_state(db: &Connection, id: &str) -> Result<RingState, NotifyError> {
    db.query_row(
        "SELECT id,phase,answered_by,muted_by FROM rings WHERE id=?1",
        params![id],
        |row| {
            Ok(RingState {
                id: row.get(0)?,
                phase: parse_phase(&row.get::<_, String>(1)?),
                answered_by: row.get(2)?,
                muted_by: row.get(3)?,
            })
        },
    )
    .optional()?
    .ok_or(NotifyError::NotFound)
}

fn cleanup_scope_name(scope: CleanupScope) -> &'static str {
    match scope {
        CleanupScope::Read => "read",
        CleanupScope::Shown => "shown",
        CleanupScope::Held => "held",
        CleanupScope::All => "all",
        CleanupScope::OlderThan => "older-than",
        CleanupScope::App => "app",
        CleanupScope::Category => "category",
        CleanupScope::Archive => "archive",
    }
}

pub fn parse_cleanup_scope(value: &str) -> Option<CleanupScope> {
    Some(match value {
        "read" => CleanupScope::Read,
        "shown" => CleanupScope::Shown,
        "held" => CleanupScope::Held,
        "all" => CleanupScope::All,
        "older-than" => CleanupScope::OlderThan,
        "app" => CleanupScope::App,
        "category" => CleanupScope::Category,
        "archive" => CleanupScope::Archive,
        _ => return None,
    })
}

fn priority_name(priority: Priority) -> &'static str {
    match priority {
        Priority::Passive => "passive",
        Priority::Active => "active",
        Priority::Urgent => "urgent",
    }
}

fn parse_priority(value: &str) -> Priority {
    match value {
        "urgent" => Priority::Urgent,
        "active" => Priority::Active,
        _ => Priority::Passive,
    }
}

fn ring_kind_name(kind: RingKind) -> &'static str {
    match kind {
        RingKind::Call => "call",
        RingKind::Doorbell => "doorbell",
        RingKind::Intercom => "intercom",
        RingKind::Alarm => "alarm",
        RingKind::Timer => "timer",
        RingKind::Pager => "pager",
    }
}

fn ring_phase_name(phase: RingPhase) -> &'static str {
    match phase {
        RingPhase::Ringing => "ringing",
        RingPhase::Accepted => "accepted",
        RingPhase::Declined => "declined",
        RingPhase::Ended => "ended",
        RingPhase::Missed => "missed",
    }
}

fn parse_phase(value: &str) -> RingPhase {
    match value {
        "accepted" => RingPhase::Accepted,
        "declined" => RingPhase::Declined,
        "ended" => RingPhase::Ended,
        "missed" => RingPhase::Missed,
        _ => RingPhase::Ringing,
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// A bounded LLM-ready projection. Sensitive bodies and entities are omitted.
pub fn llm_context(envelope: &Envelope) -> serde_json::Value {
    let context = envelope.context.as_ref();
    serde_json::json!({
        "id": envelope.id,
        "app_id": envelope.app_id,
        "category": envelope.category,
        "priority": priority_name(envelope.priority),
        "created_at_ms": envelope.created_at_ms,
        "title": if envelope.sensitive { "[sensitive]" } else { envelope.title.as_str() },
        "summary": if envelope.sensitive {
            "[sensitive]".to_owned()
        } else {
            context.and_then(|context| context.summary.clone()).unwrap_or_default()
        },
        "entities": if envelope.sensitive {
            Vec::<String>::new()
        } else {
            context.map(|context| context.entities.clone()).unwrap_or_default()
        },
        "topics": context.map(|context| context.topics.clone()).unwrap_or_default(),
    })
}

/// Dedupe helper used by a relay or recipient host.
pub fn dedupe_ids(ids: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    ids.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(id: &str, priority: Priority) -> Envelope {
        Envelope {
            id: id.into(),
            app_id: "dev.fungos.test".into(),
            source_device: "mac".into(),
            title: "Build failed".into(),
            body: "soil-runtime".into(),
            category: "build".into(),
            thread: Some("ci".into()),
            group_key: None,
            created_at_ms: now_ms(),
            expires_at_ms: None,
            priority,
            sensitive: false,
            actions: vec![Action {
                id: "open".into(),
                label: "Open".into(),
                destructive: false,
            }],
            context: None,
        }
    }

    #[test]
    fn publisher_cannot_promote_to_urgent() {
        let broker = NotifyBroker::in_memory("mac").unwrap();
        broker.publish(envelope("n1", Priority::Urgent)).unwrap();
        let items = broker.inbox(&Filter::default()).unwrap();
        assert_eq!(items[0].envelope.priority, Priority::Active);
    }

    #[test]
    fn history_is_denied_until_enabled() {
        let mut broker = NotifyBroker::in_memory("mac").unwrap();
        broker.publish(envelope("n1", Priority::Passive)).unwrap();
        assert!(matches!(broker.history(0, 10), Err(NotifyError::Denied)));
        broker.set_history_enabled(true);
        assert_eq!(broker.history(0, 10).unwrap().len(), 1);
    }

    #[test]
    fn action_and_cleanup_enqueue_peer_state() {
        let mut broker = NotifyBroker::in_memory("mac").unwrap();
        broker.set_peers(["studio".to_owned()]);
        broker.publish(envelope("n1", Priority::Active)).unwrap();
        broker.act("n1", "open").unwrap();
        let pending = broker.pending_outbox(now_ms() + 1, 100).unwrap();
        assert!(pending.iter().any(|row| {
            row.kind == NOTIFY_STATE && row.payload.contains("\"acted_action\":\"open\"")
        }));
        broker.dismiss("n1").unwrap();
        let receipt = broker.cleanup(CleanupScope::Read, None, None).unwrap();
        let pending = broker.pending_outbox(now_ms() + 1, 100).unwrap();
        assert!(
            pending
                .iter()
                .any(|row| { row.kind == NOTIFY_CLEANUP && row.payload.contains(&receipt.id) })
        );
        broker.undo_cleanup(&receipt.id).unwrap();
        let pending = broker.pending_outbox(now_ms() + 1, 100).unwrap();
        assert!(pending.iter().any(|row| row.kind == NOTIFY_CLEANUP_UNDO));
    }

    #[test]
    fn focus_holds_instead_of_deleting() {
        let broker = NotifyBroker::in_memory("mac").unwrap();
        broker.publish(envelope("n1", Priority::Active)).unwrap();
        broker
            .set_focus(FocusProfile {
                mode: "focus".into(),
                until_ms: None,
                allow_apps: Vec::new(),
                allow_categories: Vec::new(),
                allow_threads: Vec::new(),
                breakthrough: Vec::new(),
                rollup_held: true,
            })
            .unwrap();
        assert!(broker.inbox(&Filter::default()).unwrap().is_empty());
        let held = broker
            .inbox(&Filter {
                include_held: true,
                ..Filter::default()
            })
            .unwrap();
        assert_eq!(held.len(), 1);
        assert!(held[0].held);
    }

    #[test]
    fn cleanup_is_undoable() {
        let broker = NotifyBroker::in_memory("mac").unwrap();
        broker.publish(envelope("n1", Priority::Passive)).unwrap();
        broker.dismiss("n1").unwrap();
        let receipt = broker.cleanup(CleanupScope::Read, None, None).unwrap();
        assert_eq!(receipt.cleared, 1);
        assert!(broker.inbox(&Filter::default()).unwrap().is_empty());
        assert_eq!(broker.undo_cleanup(&receipt.id).unwrap(), 1);
        assert_eq!(
            broker
                .inbox(&Filter {
                    include_read: true,
                    ..Filter::default()
                })
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn remote_envelope_is_deduped() {
        let broker = NotifyBroker::in_memory("mac").unwrap();
        let envelope = envelope("n1", Priority::Passive);
        assert!(broker.apply_remote(envelope.clone()).unwrap());
        assert!(!broker.apply_remote(envelope).unwrap());
        assert_eq!(broker.inbox(&Filter::default()).unwrap().len(), 1);
    }

    #[test]
    fn only_one_device_can_accept_a_ring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify.db");
        let mac = NotifyBroker::open(&path, "mac").unwrap();
        let studio = NotifyBroker::open(&path, "studio").unwrap();
        let ring = Ring {
            id: "r1".into(),
            kind: RingKind::Call,
            source: "ada".into(),
            title: "Ada".into(),
            priority: Priority::Urgent,
            created_at_ms: now_ms(),
            expires_at_ms: None,
            actions: Vec::new(),
        };
        mac.ring(ring).unwrap();
        assert_eq!(mac.accept_ring("r1").unwrap().phase, RingPhase::Accepted);
        assert!(matches!(
            studio.accept_ring("r1"),
            Err(NotifyError::AlreadyAnswered)
        ));
    }

    #[test]
    fn outbox_retries_until_delivered() {
        struct FailOnce(std::sync::atomic::AtomicBool);
        impl PeerRouter for FailOnce {
            fn deliver(&self, _peer: &str, _kind: &str, _payload: &str) -> Result<(), String> {
                if self.0.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    Err("offline".into())
                } else {
                    Ok(())
                }
            }
        }
        let broker = NotifyBroker::in_memory("mac").unwrap();
        broker.publish(envelope("n1", Priority::Passive)).unwrap();
        let router = FailOnce(std::sync::atomic::AtomicBool::new(true));
        assert_eq!(broker.flush_outbox(&router, now_ms(), 10).unwrap(), 0);
        assert_eq!(
            broker.pending_outbox(now_ms() + 10_000, 10).unwrap().len(),
            1
        );
        assert_eq!(
            broker.flush_outbox(&router, now_ms() + 10_000, 10).unwrap(),
            1
        );
        assert!(
            broker
                .pending_outbox(now_ms() + 20_000, 10)
                .unwrap()
                .is_empty()
        );
    }
}
