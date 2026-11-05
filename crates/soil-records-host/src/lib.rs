//! Contacts records host for `soil:records`.
//!
//! This adapter exposes one `fungos-records` book as one `soil:records`
//! collection. Local CRUD and scan are implemented. Remote `changes`/`apply`
//! currently return `unsupported`; the native Unibus provider remains the sync
//! path.

use std::sync::Mutex;

use anyhow::Context;
use fungos_records::{Contact, Record, Store};
use wasmtime::component::{HasSelf, Linker};

wasmtime::component::bindgen!({ path: "wit", world: "records-host" });

use soil::records::records as records;

const MAX_PAGE: u32 = 512;
const CONTACT_SCHEMA: &str = "fungos.contacts/contact";

pub struct ContactsRecordsHost {
    store: Mutex<Store>,
    book: String,
    collection: String,
}

impl ContactsRecordsHost {
    pub fn open(
        root: impl AsRef<std::path::Path>,
        book: impl Into<String>,
        collection: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let root = root.as_ref();
        let device = root.join("device_id");
        let store = Store::open(root, &device).context("open fungos-records")?;
        Ok(Self {
            store: Mutex::new(store),
            book: book.into(),
            collection: collection.into(),
        })
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
            origin: self
                .store
                .lock()
                .expect("records store poisoned")
                .device_id
                .clone(),
            deleted: record.deleted,
            body,
        }
    }
}

impl records::Host for ContactsRecordsHost {
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
        let cursor = cursor.unwrap_or_default();
        let records = {
            let store = self.store.lock().expect("records store poisoned");
            store
                .list(&self.book, None, usize::MAX)
                .map_err(|_| records::Error::Io)?
        };
        let mut entries: Vec<_> = records
            .into_iter()
            .filter(|record| record.entity_id > cursor)
            .take(limit as usize + 1)
            .collect();
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
        Ok(records::Page { entries: page, next })
    }

    fn upsert(&mut self, entry: records::Entry) -> Result<records::Entry, records::Error> {
        let id = self.key(&entry.key)?;
        if entry.schema != CONTACT_SCHEMA || entry.body.is_empty() {
            return Err(records::Error::Invalid);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&entry.body).map_err(|_| records::Error::Invalid)?;
        // Refuse arbitrary bodies that do not project to a contact.
        let _: Contact = serde_json::from_value(body.clone()).map_err(|_| records::Error::Invalid)?;
        let mut store = self.store.lock().expect("records store poisoned");
        store
            .append(&self.book, &id, "contact.put", body)
            .map_err(|_| records::Error::Io)?;
        let record = store
            .get(&self.book, &id)
            .map_err(|_| records::Error::Io)?;
        drop(store);
        Ok(self.entry(record))
    }

    fn delete(&mut self, key: records::Key) -> Result<(), records::Error> {
        let id = self.key(&key)?;
        let mut store = self.store.lock().expect("records store poisoned");
        store
            .append(&self.book, &id, "contact.delete", serde_json::Value::Null)
            .map_err(|_| records::Error::Io)?;
        Ok(())
    }

    fn changes(
        &mut self,
        _since: Option<String>,
        _limit: u32,
    ) -> Result<records::Page, records::Error> {
        Err(records::Error::Unsupported)
    }

    fn apply(
        &mut self,
        _entries: Vec<records::Entry>,
    ) -> Result<records::ApplyResult, records::Error> {
        Err(records::Error::Unsupported)
    }
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
    use crate::soil::records::records::Host as _;

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
    fn upsert_get_scan_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = ContactsRecordsHost::open(dir.path(), "personal/test/contacts", "contact").unwrap();
        let inserted = host.upsert(contact("ada")).unwrap();
        assert_eq!(inserted.revision, inserted.updated_at_ms);
        assert_eq!(host.get(records::Key { collection: "contact".into(), id: "ada".into() }).unwrap().key.id, "ada");

        let page = host.scan("contact".into(), None, 10).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert!(page.next.is_none());

        host.delete(records::Key { collection: "contact".into(), id: "ada".into() }).unwrap();
        let page = host.scan("contact".into(), None, 10).unwrap();
        assert!(page.entries.is_empty());
    }
}
