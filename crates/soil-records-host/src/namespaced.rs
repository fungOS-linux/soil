//! Namespaced `soil:records` host.
//!
//! One Spore can read several books without seeing host paths. Each namespace is
//! a named grant:
//!
//! ```text
//! crm       -> team/fpl/crm        read/write
//! contacts  -> personal/avery/contacts read
//! messages  -> personal/avery/mail read
//! ```
//!
//! The Spore names only `crm`, `contacts`, `messages`. The host maps those names
//! to books and enforces read/write per namespace. Roles/Mycelium determine
//! which namespace grants are linked.

use std::{collections::BTreeMap, sync::Mutex};

use anyhow::Context;
use fungos_records::{Record, Store};

use crate::soil::records::records;

const MAX_PAGE: u32 = 512;

/// One named book grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace {
    pub name: String,
    pub book: String,
    pub collection: String,
    pub read: bool,
    pub write: bool,
}

impl Namespace {
    pub fn read(
        name: impl Into<String>,
        book: impl Into<String>,
        collection: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            book: book.into(),
            collection: collection.into(),
            read: true,
            write: false,
        }
    }

    pub fn write(
        name: impl Into<String>,
        book: impl Into<String>,
        collection: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            book: book.into(),
            collection: collection.into(),
            read: true,
            write: true,
        }
    }
}

pub struct NamespacedRecordsHost {
    store: Mutex<Store>,
    namespaces: BTreeMap<String, Namespace>,
}

impl NamespacedRecordsHost {
    pub fn open(
        root: impl AsRef<std::path::Path>,
        namespaces: Vec<Namespace>,
    ) -> anyhow::Result<Self> {
        let root = root.as_ref();
        let store = Store::open(root, &root.join("device_id")).context("open fungos-records")?;
        let mut map = BTreeMap::new();
        for namespace in namespaces {
            if namespace.name.is_empty()
                || namespace.book.is_empty()
                || namespace.collection.is_empty()
            {
                anyhow::bail!("namespace name, book and collection are required");
            }
            if map.insert(namespace.name.clone(), namespace).is_some() {
                anyhow::bail!("duplicate namespace");
            }
        }
        Ok(Self {
            store: Mutex::new(store),
            namespaces: map,
        })
    }

    pub fn namespaces(&self) -> impl Iterator<Item = &Namespace> {
        self.namespaces.values()
    }

    fn namespace(&self, name: &str, write: bool) -> Result<&Namespace, records::Error> {
        let namespace = self.namespaces.get(name).ok_or(records::Error::Denied)?;
        if !namespace.read || (write && !namespace.write) {
            return Err(records::Error::Denied);
        }
        Ok(namespace)
    }

    fn entry(namespace: &Namespace, record: Record) -> records::Entry {
        records::Entry {
            key: records::Key {
                collection: namespace.name.clone(),
                id: record.entity_id,
            },
            schema: record.schema_id,
            revision: record.updated_hlc,
            updated_at_ms: record.updated_hlc,
            origin: record.origin,
            deleted: record.deleted,
            body: serde_json::to_vec(&record.fields).unwrap_or_default(),
        }
    }
}

impl records::Host for NamespacedRecordsHost {
    fn get(&mut self, key: records::Key) -> Result<records::Entry, records::Error> {
        let namespace = self.namespace(&key.collection, false)?;
        if key.id.is_empty() {
            return Err(records::Error::Invalid);
        }
        let record = {
            let store = self.store.lock().expect("records store poisoned");
            store
                .get(&namespace.book, &key.id)
                .map_err(|_| records::Error::NotFound)?
        };
        Ok(Self::entry(namespace, record))
    }

    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<records::Page, records::Error> {
        let namespace = self.namespace(&collection, false)?;
        if limit == 0 || limit > MAX_PAGE {
            return Err(records::Error::OverBudget);
        }
        let records = {
            let store = self.store.lock().expect("records store poisoned");
            store
                .scan_records(&namespace.book, cursor.as_deref(), limit as usize + 1)
                .map_err(|_| records::Error::Io)?
        };
        let mut entries: Vec<_> = records.into_iter().collect();
        let next = if entries.len() > limit as usize {
            entries.truncate(limit as usize);
            entries.last().map(|record| record.entity_id.clone())
        } else {
            None
        };
        Ok(records::Page {
            entries: entries
                .into_iter()
                .map(|record| Self::entry(namespace, record))
                .collect(),
            next,
        })
    }

    fn upsert(&mut self, entry: records::Entry) -> Result<records::Entry, records::Error> {
        let namespace = self.namespace(&entry.key.collection, true)?;
        if entry.key.id.is_empty() || entry.schema.is_empty() {
            return Err(records::Error::Invalid);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&entry.body).map_err(|_| records::Error::Invalid)?;
        if !value.is_object() {
            return Err(records::Error::Invalid);
        }
        let mut store = self.store.lock().expect("records store poisoned");
        store
            .put_record(&namespace.book, &entry.key.id, &entry.schema, 1, value)
            .map_err(|_| records::Error::Io)?;
        let record = store
            .get(&namespace.book, &entry.key.id)
            .map_err(|_| records::Error::Io)?;
        drop(store);
        Ok(Self::entry(namespace, record))
    }

    fn compare_and_swap(
        &mut self,
        key: records::Key,
        expected: Option<records::Version>,
        entry: records::Entry,
    ) -> Result<records::CasResult, records::Error> {
        let namespace = self.namespace(&key.collection, true)?;
        if entry.key.collection != key.collection || entry.key.id != key.id {
            return Err(records::Error::Invalid);
        }
        if key.id.is_empty() || entry.schema.is_empty() {
            return Err(records::Error::Invalid);
        }
        let mut store = self.store.lock().expect("records store poisoned");
        let current = store.get(&namespace.book, &key.id).ok();
        let current_entry = current
            .as_ref()
            .map(|record| Self::entry(namespace, record.clone()));
        let matches = match (&expected, &current) {
            (None, None) => true,
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
        if entry.deleted {
            let schema = if entry.schema.is_empty() {
                current
                    .as_ref()
                    .map(|record| record.schema_id.clone())
                    .ok_or(records::Error::Invalid)?
            } else {
                entry.schema.clone()
            };
            store
                .delete_record(&namespace.book, &key.id, &schema, 1)
                .map_err(|_| records::Error::Io)?;
        } else {
            let value: serde_json::Value =
                serde_json::from_slice(&entry.body).map_err(|_| records::Error::Invalid)?;
            if !value.is_object() {
                return Err(records::Error::Invalid);
            }
            store
                .put_record(&namespace.book, &key.id, &entry.schema, 1, value)
                .map_err(|_| records::Error::Io)?;
        }
        let record = store
            .get(&namespace.book, &key.id)
            .map_err(|_| records::Error::Io)?;
        drop(store);
        Ok(records::CasResult {
            applied: true,
            entry: Self::entry(namespace, record),
        })
    }

    fn delete(&mut self, key: records::Key) -> Result<(), records::Error> {
        let namespace = self.namespace(&key.collection, true)?;
        if key.id.is_empty() {
            return Err(records::Error::Invalid);
        }
        let mut store = self.store.lock().expect("records store poisoned");
        let schema = store
            .get(&namespace.book, &key.id)
            .map(|record| record.schema_id)
            .map_err(|_| records::Error::NotFound)?;
        store
            .delete_record(&namespace.book, &key.id, &schema, 1)
            .map_err(|_| records::Error::Io)?;
        Ok(())
    }

    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> Result<records::Page, records::Error> {
        // A global cursor across multiple books is ambiguous. A single-book
        // host keeps the ordinary changes semantics; multi-book sync is a
        // host-mediated operation, not a Spore operation.
        if self.namespaces.len() != 1 {
            return Err(records::Error::Unsupported);
        }
        if limit == 0 || limit > MAX_PAGE {
            return Err(records::Error::OverBudget);
        }
        let namespace = self.namespaces.values().next().expect("one namespace");
        let records = {
            let store = self.store.lock().expect("records store poisoned");
            store
                .scan_changes(&namespace.book, since.as_deref(), limit as usize + 1)
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
        Ok(records::Page {
            entries: entries
                .into_iter()
                .map(|record| Self::entry(namespace, record))
                .collect(),
            next,
        })
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
            let namespace = self.namespace(&entry.key.collection, true)?;
            if entry.key.id.is_empty() || entry.schema.is_empty() {
                return Err(records::Error::Invalid);
            }
            let fields = if entry.deleted {
                serde_json::json!({})
            } else {
                let value: serde_json::Value =
                    serde_json::from_slice(&entry.body).map_err(|_| records::Error::Invalid)?;
                if !value.is_object() {
                    return Err(records::Error::Invalid);
                }
                value
            };
            let applied = store
                .apply_entry(
                    &namespace.book,
                    fungos_records::RecordEntry {
                        entity_id: &entry.key.id,
                        schema_id: &entry.schema,
                        schema_version: 1,
                        fields,
                        deleted: entry.deleted,
                        revision: entry.revision,
                        origin: &entry.origin,
                    },
                )
                .map_err(|_| records::Error::Io)?;
            if applied {
                result.applied += 1;
            } else {
                result.skipped += 1;
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soil::records::records::Host as _;

    fn host(root: &std::path::Path) -> NamespacedRecordsHost {
        NamespacedRecordsHost::open(
            root,
            vec![
                Namespace::write("crm", "team/test/crm", "crm"),
                Namespace::read("contacts", "personal/test/contacts", "contact"),
            ],
        )
        .unwrap()
    }

    fn entry(namespace: &str, id: &str) -> records::Entry {
        records::Entry {
            key: records::Key {
                collection: namespace.into(),
                id: id.into(),
            },
            schema: "fungos.crm/account@1".into(),
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body: serde_json::to_vec(&serde_json::json!({"name": "Acme"})).unwrap(),
        }
    }

    #[test]
    fn routes_namespaces_and_enforces_write_grants() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = host(dir.path());
        host.upsert(entry("crm", "a1")).unwrap();
        assert_eq!(
            host.get(records::Key {
                collection: "crm".into(),
                id: "a1".into()
            })
            .unwrap()
            .key
            .collection,
            "crm"
        );
        assert_eq!(host.scan("crm".into(), None, 10).unwrap().entries.len(), 1);
        assert_eq!(
            host.upsert(entry("contacts", "c1")).unwrap_err(),
            records::Error::Denied
        );
        assert_eq!(
            host.scan("missing".into(), None, 10).unwrap_err(),
            records::Error::Denied
        );
    }

    #[test]
    fn multi_book_changes_are_host_mediated() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = host(dir.path());
        assert_eq!(
            host.changes(None, 10).unwrap_err(),
            records::Error::Unsupported
        );
    }
}
