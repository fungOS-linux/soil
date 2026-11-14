//! Host adapter for `soil:records`.
//!
//! This exposes one `fungos-records` book as one `soil:records` collection.
//! CRUD, scan, materialized changes, and revision/origin `apply` are local
//! operations; peer transport remains the native `records.sync.*` provider.

use std::sync::Mutex;

use anyhow::Context;
use fungos_records::{Contact, Record, Store};
use wasmtime::component::{HasSelf, Linker};

mod namespaced;
pub use namespaced::{Namespace, NamespacedRecordsHost};

wasmtime::component::bindgen!({ path: "wit/records", world: "records-host" });

pub use self::soil::records::records;

/// Map a store failure to the interface's `io` error, saying why on the host's
/// own stderr first.
///
/// `soil:records` has no "other error" variant, so the cause cannot cross the
/// boundary — but dropping it entirely means a contended store reaches the
/// guest as a bare `io` and the guest can only abort. The engine that knows what
/// went wrong writes it down.
fn store_error(context: &str, error: anyhow::Error) -> records::Error {
    eprintln!("records host: {context}: {error:#}");
    records::Error::Io
}



/// Records one `scan` call may return.
///
/// Reading a 6,000-record book crossed this boundary 12 times at 512 and twice
/// at 4096, and wall-clock did not move: the cost is copying every body into the
/// component, not the call. So this is a memory knob more than a speed one, and
/// it is set high enough that a whole book is two or three crossings without
/// holding an unbounded page at once. The real lever is not fetching bodies a
/// query does not need — see the records-backend follow-ups.
const MAX_PAGE: u32 = 4096;
const CONTACT_SCHEMA: &str = "fungos.contacts/contact";

#[derive(Clone, Debug, PartialEq, Eq)]
enum SchemaPolicy {
    Any,
    Exact(String),
    Contact,
}

pub struct RecordsBookHost {
    store: Mutex<Store>,
    book: String,
    collection: String,
    schema: SchemaPolicy,
}

impl RecordsBookHost {
    /// Accept any non-empty schema in this collection.
    pub fn open(
        root: impl AsRef<std::path::Path>,
        book: impl Into<String>,
        collection: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::open_with_policy(root, book, collection, SchemaPolicy::Any)
    }

    /// Require every entry to carry this schema id.
    pub fn open_with_schema(
        root: impl AsRef<std::path::Path>,
        book: impl Into<String>,
        collection: impl Into<String>,
        schema: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::open_with_policy(root, book, collection, SchemaPolicy::Exact(schema.into()))
    }

    /// Contact-specific host: schema `fungos.contacts/contact` and vCard-shaped
    /// body validation.
    pub fn open_contacts(
        root: impl AsRef<std::path::Path>,
        book: impl Into<String>,
        collection: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::open_with_policy(root, book, collection, SchemaPolicy::Contact)
    }

    fn open_with_policy(
        root: impl AsRef<std::path::Path>,
        book: impl Into<String>,
        collection: impl Into<String>,
        schema: SchemaPolicy,
    ) -> anyhow::Result<Self> {
        let root = root.as_ref();
        let device = root.join("device_id");
        let store = Store::open(root, &device).context("open fungos-records")?;
        Ok(Self {
            store: Mutex::new(store),
            book: book.into(),
            collection: collection.into(),
            schema,
        })
    }

    pub fn book_id(&self) -> &str {
        &self.book
    }

    pub fn collection_name(&self) -> &str {
        &self.collection
    }

    /// Scan one page through the same `soil:records` path used by a Spore.
    pub fn scan_entries(
        &mut self,
        cursor: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<(Vec<records::Entry>, Option<String>)> {
        let page = <Self as records::Host>::scan(
            self,
            self.collection.clone(),
            cursor.map(str::to_owned),
            limit,
        )
        .map_err(host_error)?;
        Ok((page.entries, page.next))
    }

    /// Read one entry through the same `soil:records` path used by a Spore.
    pub fn get_entry(&mut self, id: &str) -> anyhow::Result<Option<records::Entry>> {
        let key = records::Key {
            collection: self.collection.clone(),
            id: id.to_owned(),
        };
        match <Self as records::Host>::get(self, key) {
            Ok(entry) => Ok(Some(entry)),
            Err(records::Error::NotFound) => Ok(None),
            Err(error) => Err(host_error(error)),
        }
    }

    /// Scan every live entry, bounded so a bad cursor cannot spin forever.
    pub fn scan_all(&mut self, max: usize) -> anyhow::Result<Vec<records::Entry>> {
        let mut entries = Vec::new();
        let mut cursor = None;
        loop {
            let (page, next) = self.scan_entries(cursor.as_deref(), MAX_PAGE)?;
            entries.extend(page);
            if entries.len() >= max {
                entries.truncate(max);
                break;
            }
            match next {
                Some(next) if Some(next.as_str()) != cursor.as_deref() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(entries)
    }

    fn schema_name(&self) -> Option<&str> {
        match &self.schema {
            SchemaPolicy::Any => None,
            SchemaPolicy::Exact(schema) => Some(schema),
            SchemaPolicy::Contact => Some(CONTACT_SCHEMA),
        }
    }

    fn check_schema(&self, schema: &str) -> Result<(), records::Error> {
        if schema.is_empty() {
            return Err(records::Error::Invalid);
        }
        if let Some(expected) = self.schema_name()
            && schema != expected
        {
            return Err(records::Error::Invalid);
        }
        Ok(())
    }

    fn validate_body(
        &self,
        schema: &str,
        body: &[u8],
    ) -> Result<serde_json::Value, records::Error> {
        if body.is_empty() {
            return Err(records::Error::Invalid);
        }
        let value: serde_json::Value =
            serde_json::from_slice(body).map_err(|_| records::Error::Invalid)?;
        if !value.is_object() {
            return Err(records::Error::Invalid);
        }
        if self.schema == SchemaPolicy::Contact || schema == CONTACT_SCHEMA {
            let _: Contact =
                serde_json::from_value(value.clone()).map_err(|_| records::Error::Invalid)?;
        }
        Ok(value)
    }

    fn key(&self, key: &records::Key) -> Result<String, records::Error> {
        if key.collection != self.collection || key.id.is_empty() {
            return Err(records::Error::Invalid);
        }
        Ok(key.id.clone())
    }

    fn entry(&self, record: Record) -> records::Entry {
        let body = serde_json::to_vec(&record.fields).unwrap_or_default();
        records::Entry {
            key: records::Key {
                collection: self.collection.clone(),
                id: record.entity_id,
            },
            schema: record.schema_id,
            revision: record.updated_hlc,
            updated_at_ms: record.updated_hlc,
            origin: record.origin,
            deleted: record.deleted,
            body,
        }
    }
}

impl records::Host for RecordsBookHost {
    fn get(&mut self, key: records::Key) -> Result<records::Entry, records::Error> {
        let id = self.key(&key)?;
        let record = {
            let store = self.store.lock().expect("records store poisoned");
            store
                .get(&self.book, &id)
                .map_err(|_| records::Error::NotFound)?
        };
        Ok(self.entry(record))
    }

    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<records::Page, records::Error> {
        if collection != self.collection {
            return Err(records::Error::Invalid);
        }
        if limit == 0 || limit > MAX_PAGE {
            return Err(records::Error::OverBudget);
        }
        let records = {
            let store = self.store.lock().expect("records store poisoned");
            store
                .scan_records(&self.book, cursor.as_deref(), limit as usize + 1)
                .map_err(|error| store_error("records store", error))?
        };
        let mut entries: Vec<_> = records.into_iter().collect();
        let next = if entries.len() > limit as usize {
            entries.truncate(limit as usize);
            entries.last().map(|record| record.entity_id.clone())
        } else {
            None
        };
        let page = entries
            .into_iter()
            .map(|record| self.entry(record))
            .collect();
        Ok(records::Page {
            entries: page,
            next,
        })
    }

    fn upsert(&mut self, entry: records::Entry) -> Result<records::Entry, records::Error> {
        let id = self.key(&entry.key)?;
        self.check_schema(&entry.schema)?;
        let body = self.validate_body(&entry.schema, &entry.body)?;
        let mut store = self.store.lock().expect("records store poisoned");
        store
            .put_record(&self.book, &id, &entry.schema, 1, body)
            .map_err(|error| store_error(&format!("upsert {id}"), error))?;
        let record = store
            .get(&self.book, &id)
            .map_err(|error| store_error(&format!("read back {id}"), error))?;
        drop(store);
        Ok(self.entry(record))
    }

    fn compare_and_swap(
        &mut self,
        key: records::Key,
        expected: Option<records::Version>,
        entry: records::Entry,
    ) -> Result<records::CasResult, records::Error> {
        let id = self.key(&key)?;
        if entry.key.collection != key.collection || entry.key.id != key.id {
            return Err(records::Error::Invalid);
        }
        if !entry.deleted {
            self.check_schema(&entry.schema)?;
            self.validate_body(&entry.schema, &entry.body)?;
        }

        let mut store = self.store.lock().expect("records store poisoned");
        let current = store.get(&self.book, &id).ok();
        let current_entry = current.as_ref().map(|record| self.entry(record.clone()));
        let matches = match (&expected, &current) {
            (None, None) => true,
            // A tombstone is absent for create-if-absent semantics.
            (None, Some(record)) => record.deleted,
            (Some(_), None) => return Err(records::Error::NotFound),
            (Some(version), Some(record)) => {
                record.updated_hlc == version.revision && record.origin == version.origin
            }
        };

        if !matches {
            return Ok(records::CasResult {
                applied: false,
                entry: current_entry.expect("conflict has a current entry"),
            });
        }

        let schema = if entry.schema.is_empty() {
            current
                .as_ref()
                .map(|record| record.schema_id.clone())
                .or_else(|| self.schema_name().map(str::to_owned))
                .ok_or(records::Error::Invalid)?
        } else {
            entry.schema.clone()
        };

        if entry.deleted {
            store
                .delete_record(&self.book, &id, &schema, 1)
                .map_err(|error| store_error("records store", error))?;
        } else {
            let body = self.validate_body(&schema, &entry.body)?;
            store
                .put_record(&self.book, &id, &schema, 1, body)
                .map_err(|error| store_error("records store", error))?;
        }

        let record = store.get(&self.book, &id).map_err(|error| store_error("records store", error))?;
        drop(store);
        Ok(records::CasResult {
            applied: true,
            entry: self.entry(record),
        })
    }

    fn delete(&mut self, key: records::Key) -> Result<(), records::Error> {
        let id = self.key(&key)?;
        let mut store = self.store.lock().expect("records store poisoned");
        let schema = store
            .get(&self.book, &id)
            .map(|record| record.schema_id)
            .unwrap_or_else(|_| self.schema_name().unwrap_or("unknown/record").to_string());
        store
            .delete_record(&self.book, &id, &schema, 1)
            .map_err(|error| store_error("records store", error))?;
        Ok(())
    }

    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> Result<records::Page, records::Error> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(records::Error::OverBudget);
        }
        let records = {
            let store = self.store.lock().expect("records store poisoned");
            store
                .scan_changes(&self.book, since.as_deref(), limit as usize + 1)
                .map_err(|_| records::Error::Invalid)?
        };
        let mut entries: Vec<_> = records.into_iter().collect();
        let next = if entries.len() > limit as usize {
            entries.truncate(limit as usize);
            entries
                .last()
                .map(|record| format!("{}:{}", record.updated_hlc, record.entity_id))
        } else {
            None
        };
        let entries = entries
            .into_iter()
            .map(|record| self.entry(record))
            .collect();
        Ok(records::Page { entries, next })
    }

    fn apply(
        &mut self,
        entries: Vec<records::Entry>,
    ) -> Result<records::ApplyResult, records::Error> {
        let mut result = records::ApplyResult {
            applied: 0,
            skipped: 0,
        };
        let mut store = self.store.lock().expect("records store poisoned");
        for entry in entries {
            let id = self.key(&entry.key)?;
            self.check_schema(&entry.schema)?;
            let fields = if entry.deleted {
                serde_json::json!({})
            } else {
                self.validate_body(&entry.schema, &entry.body)?
            };
            let applied = store
                .apply_entry(
                    &self.book,
                    fungos_records::RecordEntry {
                        entity_id: &id,
                        schema_id: &entry.schema,
                        schema_version: 1,
                        fields,
                        deleted: entry.deleted,
                        revision: entry.revision,
                        origin: &entry.origin,
                    },
                )
                .map_err(|error| store_error("records store", error))?;
            if applied {
                result.applied += 1;
            } else {
                result.skipped += 1;
            }
        }
        Ok(result)
    }
}

fn host_error(error: records::Error) -> anyhow::Error {
    anyhow::anyhow!("soil:records {error:?}")
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: records::Host + 'static,
{
    records::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use self::soil::records::records::Host as _;

    fn contact(id: &str) -> records::Entry {
        let value = serde_json::json!({
            "display_name": "Ada Lovelace",
            "given_name": "Ada",
            "family_name": "Lovelace",
            "organization": "",
            "title": "",
            "emails": [{"label": "work", "value": "ada@example.invalid"}],
            "phones": [],
            "addresses": [],
            "urls": [],
            "note": "",
            "birthday": "",
            "uid": id,
            "raw_vcard": ""
        });
        records::Entry {
            key: records::Key {
                collection: "contact".into(),
                id: id.into(),
            },
            schema: CONTACT_SCHEMA.into(),
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    #[test]
    fn generic_collection_accepts_any_schema() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = RecordsBookHost::open(dir.path(), "personal/test/mail", "message").unwrap();
        let entry = records::Entry {
            key: records::Key {
                collection: "message".into(),
                id: "msg-1".into(),
            },
            schema: "fungos.mail/message@1".into(),
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body: serde_json::to_vec(&serde_json::json!({"subject": "hello"})).unwrap(),
        };
        let stored = host.upsert(entry).unwrap();
        assert_eq!(stored.schema, "fungos.mail/message@1");
        assert_eq!(
            host.get(records::Key {
                collection: "message".into(),
                id: "msg-1".into(),
            })
            .unwrap()
            .key
            .id,
            "msg-1"
        );
    }

    #[test]
    fn compare_and_swap_rejects_a_stale_writer_and_allows_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut host =
            RecordsBookHost::open_contacts(dir.path(), "personal/test/contacts", "contact")
                .unwrap();
        let key = records::Key {
            collection: "contact".into(),
            id: "ada".into(),
        };
        let first = host
            .compare_and_swap(key.clone(), None, contact("ada"))
            .unwrap();
        assert!(first.applied);
        let version = records::Version {
            revision: first.entry.revision,
            origin: first.entry.origin.clone(),
        };

        let duplicate = host
            .compare_and_swap(key.clone(), None, contact("ada"))
            .unwrap();
        assert!(!duplicate.applied);
        assert_eq!(duplicate.entry.revision, version.revision);
        assert_eq!(duplicate.entry.origin, version.origin);

        let stale = host
            .compare_and_swap(
                key.clone(),
                Some(records::Version {
                    revision: version.revision.saturating_sub(1),
                    origin: version.origin.clone(),
                }),
                contact("ada"),
            )
            .unwrap();
        assert!(!stale.applied);

        let mut changed = contact("ada");
        changed.revision = 0;
        let replaced = host
            .compare_and_swap(key.clone(), Some(version), changed)
            .unwrap();
        assert!(replaced.applied);
        assert!(replaced.entry.revision > stale.entry.revision);

        let current = records::Version {
            revision: replaced.entry.revision,
            origin: replaced.entry.origin.clone(),
        };
        let mut tombstone = contact("ada");
        tombstone.deleted = true;
        tombstone.body.clear();
        let deleted = host
            .compare_and_swap(key.clone(), Some(current), tombstone)
            .unwrap();
        assert!(deleted.applied);
        assert!(deleted.entry.deleted);
        assert!(
            host.scan("contact".into(), None, 10)
                .unwrap()
                .entries
                .is_empty()
        );

        assert_eq!(
            host.compare_and_swap(
                records::Key {
                    collection: "contact".into(),
                    id: "missing".into(),
                },
                Some(records::Version {
                    revision: 1,
                    origin: "x".into(),
                }),
                contact("missing"),
            )
            .unwrap_err(),
            records::Error::NotFound
        );
    }

    #[test]
    fn changes_paginate_tombstones_and_apply_across_hosts() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let mut source =
            RecordsBookHost::open_contacts(source_dir.path(), "personal/test/contacts", "contact")
                .unwrap();
        let mut target =
            RecordsBookHost::open_contacts(target_dir.path(), "personal/test/contacts", "contact")
                .unwrap();
        source.upsert(contact("ada")).unwrap();
        source.upsert(contact("grace")).unwrap();

        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = source.changes(cursor.clone(), 1).unwrap();
            seen.extend(page.entries.iter().map(|entry| entry.key.id.clone()));
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen, ["ada", "grace"]);

        let all = source.changes(None, 10).unwrap();
        let applied = target.apply(all.entries).unwrap();
        assert_eq!(applied.applied, 2);
        assert_eq!(applied.skipped, 0);
        assert_eq!(
            target
                .get(records::Key {
                    collection: "contact".into(),
                    id: "ada".into()
                })
                .unwrap()
                .key
                .id,
            "ada"
        );

        source
            .delete(records::Key {
                collection: "contact".into(),
                id: "ada".into(),
            })
            .unwrap();
        let changes = source.changes(None, 10).unwrap();
        assert!(
            changes
                .entries
                .iter()
                .any(|entry| entry.key.id == "ada" && entry.deleted)
        );
        let applied = target.apply(changes.entries).unwrap();
        assert_eq!(applied.applied, 1);
        let page = target.scan("contact".into(), None, 10).unwrap();
        assert_eq!(page.entries.len(), 1);
    }

    #[test]
    fn upsert_get_scan_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut host =
            RecordsBookHost::open_contacts(dir.path(), "personal/test/contacts", "contact")
                .unwrap();
        let inserted = host.upsert(contact("ada")).unwrap();
        assert_eq!(inserted.revision, inserted.updated_at_ms);
        assert_eq!(
            host.get(records::Key {
                collection: "contact".into(),
                id: "ada".into()
            })
            .unwrap()
            .key
            .id,
            "ada"
        );

        let page = host.scan("contact".into(), None, 10).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert!(page.next.is_none());

        host.delete(records::Key {
            collection: "contact".into(),
            id: "ada".into(),
        })
        .unwrap();
        let page = host.scan("contact".into(), None, 10).unwrap();
        assert!(page.entries.is_empty());
    }
}
