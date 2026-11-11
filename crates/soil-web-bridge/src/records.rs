//! `soil:records` over the bridge.
//!
//! The capability is a thin adapter over [`RecordsBookHost`] — the *same* type
//! the wasmtime host hands a Spore. That is deliberate: `revision` is an HLC, a
//! tombstone stays visible to `get`, `scan` pages by entity id, and bodies are
//! JSON objects. Re-implementing any of that here would be a second answer to a
//! question the host already answers, and the two would drift the first time a
//! record changed shape.
//!
//! Bodies cross the wire as base64, because WIT says `list<u8>` and OpenAPI says
//! `format: byte`. This backend stores JSON, so a body that is not a JSON object
//! is `invalid` — that is the backend's constraint, not the bridge's, and it is
//! stated once here instead of being rediscovered by every client.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use soil_records_host::{RecordsBookHost, records};

use crate::protocol::{CallError, Code, Event};
use crate::{CallOutput, Capability};

/// A body larger than this is refused before it reaches SQLite.
///
/// The WIT has no ceiling of its own; `soil:records` says the host owns the
/// budget and reports `over-budget`. This is that budget.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// The page size used when a client omits `limit`. The WIT requires one; a
/// hand-written curl is friendlier with a default.
pub const DEFAULT_PAGE: u32 = 100;

pub struct RecordsCapability {
    host: Mutex<RecordsBookHost>,
    collection: String,
    book: String,
}

impl RecordsCapability {
    /// Open one book as one collection.
    ///
    /// One bridge instance serves one collection, for the same reason
    /// [`RecordsBookHost`] does: a namespace is host configuration, not a
    /// client-chosen path. A page that asks for another collection is `invalid`.
    pub fn open(
        root: &Path,
        book: impl Into<String>,
        collection: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let book = book.into();
        let collection = collection.into();
        let host = RecordsBookHost::open(root, book.clone(), collection.clone())?;
        Ok(Self {
            host: Mutex::new(host),
            collection,
            book,
        })
    }

    pub fn book(&self) -> &str {
        &self.book
    }

    pub fn collection(&self) -> &str {
        &self.collection
    }

    fn host(&self) -> Result<MutexGuard<'_, RecordsBookHost>, CallError> {
        self.host
            .lock()
            .map_err(|_| CallError::io("records store is poisoned"))
    }

    fn check_collection(&self, collection: &str) -> Result<(), CallError> {
        if collection != self.collection {
            return Err(CallError::invalid(format!(
                "this bridge serves collection {:?}, not {collection:?}",
                self.collection
            )));
        }
        Ok(())
    }

    fn get(&self, params: &Value) -> Result<CallOutput, CallError> {
        let params: GetParams = parse("get", params)?;
        self.check_collection(&params.key.collection)?;
        let key = params.key.into_records(&self.collection);
        let entry = records::Host::get(&mut *self.host()?, key).map_err(map_error)?;
        // The result is the WIT result value, unwrapped: `get` returns `entry`,
        // so `{"ok": {…entry…}}` and nothing invented in between. A generated
        // client can then type `ok` as exactly the WIT type.
        Ok(CallOutput::value(json!(WireEntry::from(entry))))
    }

    fn scan(&self, params: &Value) -> Result<CallOutput, CallError> {
        let params: ScanParams = parse("scan", params)?;
        self.check_collection(&params.collection)?;
        let limit = params.limit.unwrap_or(DEFAULT_PAGE);
        let page = records::Host::scan(
            &mut *self.host()?,
            self.collection.clone(),
            params.cursor,
            limit,
        )
        .map_err(map_error)?;
        let entries: Vec<WireEntry> = page.entries.into_iter().map(WireEntry::from).collect();
        Ok(CallOutput::value(json!({
            "entries": entries,
            "next": page.next,
        })))
    }

    fn upsert(&self, params: &Value) -> Result<CallOutput, CallError> {
        let params: UpsertParams = parse("upsert", params)?;
        let entry = params.entry.into_records(&self.collection)?;
        check_body(&entry)?;
        let stored = records::Host::upsert(&mut *self.host()?, entry).map_err(map_error)?;
        let id = stored.key.id.clone();
        let revision = stored.revision;
        Ok(CallOutput::with_event(
            json!(WireEntry::from(stored)),
            Event::Changed {
                collection: self.collection.clone(),
                id,
                revision,
            },
        ))
    }

    fn delete(&self, params: &Value) -> Result<CallOutput, CallError> {
        let params: DeleteParams = parse("delete", params)?;
        self.check_collection(&params.key.collection)?;
        let key = params.key.into_records(&self.collection);
        let id = key.id.clone();
        let mut host = self.host()?;
        records::Host::delete(&mut *host, key.clone()).map_err(map_error)?;
        // The write returns nothing, so re-read the tombstone for the revision a
        // watcher needs to tell "new" from "deleted".
        let revision = records::Host::get(&mut *host, key)
            .map(|entry| entry.revision)
            .unwrap_or_default();
        drop(host);
        Ok(CallOutput::with_event(
            json!({}),
            Event::Changed {
                collection: self.collection.clone(),
                id,
                revision,
            },
        ))
    }

    fn compare_and_swap(&self, params: &Value) -> Result<CallOutput, CallError> {
        let params: CasParams = parse("compare-and-swap", params)?;
        self.check_collection(&params.key.collection)?;
        let key = params.key.into_records(&self.collection);
        let entry = params.entry.into_records(&self.collection)?;
        check_body(&entry)?;
        let expected = params.expected.map(|version| records::Version {
            revision: version.revision,
            origin: version.origin,
        });
        let result = records::Host::compare_and_swap(&mut *self.host()?, key, expected, entry)
            .map_err(map_error)?;
        let applied = result.applied;
        let id = result.entry.key.id.clone();
        let revision = result.entry.revision;
        let value = json!({
            "applied": applied,
            "entry": WireEntry::from(result.entry),
        });
        if applied {
            return Ok(CallOutput::with_event(
                value,
                Event::Changed {
                    collection: self.collection.clone(),
                    id,
                    revision,
                },
            ));
        }
        Ok(CallOutput::value(value))
    }

    fn changes(&self, params: &Value) -> Result<CallOutput, CallError> {
        let params: ChangesParams = parse("changes", params)?;
        let limit = params.limit.unwrap_or(DEFAULT_PAGE);
        let page =
            records::Host::changes(&mut *self.host()?, params.since, limit).map_err(map_error)?;
        let entries: Vec<WireEntry> = page.entries.into_iter().map(WireEntry::from).collect();
        Ok(CallOutput::value(json!({
            "entries": entries,
            "next": page.next,
        })))
    }

    fn apply(&self, params: &Value) -> Result<CallOutput, CallError> {
        let params: ApplyParams = parse("apply", params)?;
        let mut entries = Vec::with_capacity(params.entries.len());
        for entry in params.entries {
            let entry = entry.into_records(&self.collection)?;
            check_body(&entry)?;
            entries.push(entry);
        }
        // A merged page can carry hundreds of entries; the change notification
        // is per-record, so a watcher re-reads on the next scan instead of the
        // host synthesising a burst of events nobody asked for.
        let result = records::Host::apply(&mut *self.host()?, entries).map_err(map_error)?;
        Ok(CallOutput::value(json!({
            "applied": result.applied,
            "skipped": result.skipped,
        })))
    }
}

impl Capability for RecordsCapability {
    fn package(&self) -> &str {
        "soil:records"
    }

    /// The WIT comment is the source of this table: "Grants are separate: read,
    /// write, and sync. A read-only Spore cannot mutate or export the store."
    fn grant(&self, function: &str) -> Option<&'static str> {
        match function {
            "get" | "scan" => Some("read"),
            "upsert" | "delete" | "compare-and-swap" => Some("write"),
            "changes" | "apply" => Some("sync"),
            _ => None,
        }
    }

    fn call(&self, function: &str, params: &Value) -> Result<CallOutput, CallError> {
        match function {
            "get" => self.get(params),
            "scan" => self.scan(params),
            "upsert" => self.upsert(params),
            "delete" => self.delete(params),
            "compare-and-swap" => self.compare_and_swap(params),
            "changes" => self.changes(params),
            "apply" => self.apply(params),
            other => Err(CallError::unsupported(format!(
                "soil:records has no function {other:?}"
            ))),
        }
    }

    fn describe(&self) -> Value {
        json!({ "book": &self.book, "collection": &self.collection })
    }
}

fn parse<T: serde::de::DeserializeOwned>(function: &str, params: &Value) -> Result<T, CallError> {
    serde_json::from_value(params.clone())
        .map_err(|error| CallError::invalid(format!("bad params for {function}: {error}")))
}

fn check_body(entry: &records::Entry) -> Result<(), CallError> {
    if entry.body.len() > MAX_BODY_BYTES {
        return Err(CallError::over_budget(format!(
            "body is {} bytes, over the {MAX_BODY_BYTES} byte budget",
            entry.body.len()
        )));
    }
    Ok(())
}

fn map_error(error: records::Error) -> CallError {
    let (code, message) = match error {
        records::Error::Denied => (Code::Denied, "the Spore has no grant for this call"),
        records::Error::NotFound => (Code::NotFound, "no such record"),
        records::Error::OverBudget => (Code::OverBudget, "record, page or body is over budget"),
        records::Error::Invalid => (Code::Invalid, "key, schema, cursor or body is malformed"),
        records::Error::Stale => (Code::Stale, "cursor predates a compaction"),
        records::Error::Io => (Code::Io, "the records store failed"),
        records::Error::Unsupported => (Code::Unsupported, "this host has no records backend"),
    };
    CallError::new(code, message)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireKey {
    pub collection: String,
    pub id: String,
}

impl WireKey {
    fn into_records(self, collection: &str) -> records::Key {
        records::Key {
            collection: collection.to_owned(),
            id: self.id,
        }
    }
}

/// One entry on the wire.
///
/// Write-only fields (`revision`, `updatedAtMs`, `origin`, `deleted`) default,
/// because `upsert` assigns them: the manifest is the authority on what a
/// client may say, and this interface says a writer does not get to claim a
/// revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireEntry {
    pub key: WireKey,
    pub schema: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub deleted: bool,
    /// Base64 of the record body. This backend requires a JSON object.
    pub body: String,
}

impl From<records::Entry> for WireEntry {
    fn from(entry: records::Entry) -> Self {
        Self {
            key: WireKey {
                collection: entry.key.collection,
                id: entry.key.id,
            },
            schema: entry.schema,
            revision: entry.revision,
            updated_at_ms: entry.updated_at_ms,
            origin: entry.origin,
            deleted: entry.deleted,
            body: STANDARD.encode(entry.body),
        }
    }
}

impl WireEntry {
    fn into_records(self, collection: &str) -> Result<records::Entry, CallError> {
        if self.key.collection != collection {
            return Err(CallError::invalid(format!(
                "entry collection {:?} is not {collection:?}",
                self.key.collection
            )));
        }
        if self.key.id.is_empty() {
            return Err(CallError::invalid("entry key needs an id"));
        }
        let body = STANDARD
            .decode(self.body.as_bytes())
            .map_err(|error| CallError::invalid(format!("body is not base64: {error}")))?;
        Ok(records::Entry {
            key: records::Key {
                collection: collection.to_owned(),
                id: self.key.id,
            },
            schema: self.schema,
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            // `deleted` is meaningful for `compare-and-swap` (a tombstone is a
            // CAS delete); `upsert` ignores it.
            deleted: self.deleted,
            body,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetParams {
    key: WireKey,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScanParams {
    collection: String,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpsertParams {
    entry: WireEntry,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteParams {
    key: WireKey,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireVersion {
    pub revision: u64,
    pub origin: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CasParams {
    key: WireKey,
    #[serde(default)]
    expected: Option<WireVersion>,
    entry: WireEntry,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChangesParams {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyParams {
    entries: Vec<WireEntry>,
}
