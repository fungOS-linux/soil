//! Guest SDK for `soil:records`.
//!
//! The host owns durability, revisions, tombstones, and sync. A Spore owns the
//! collection name, schema, and body encoding.

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
    generate_all,
});

use std::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};
use soil::records::records::{self, Entry, Key, Page};
use soil_sdk_codec::{Codec, JsonCodec};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("records access denied")]
    Denied,
    #[error("record not found")]
    NotFound,
    #[error("records budget exceeded")]
    OverBudget,
    #[error("invalid record, cursor, or schema")]
    Invalid,
    #[error("records cursor is stale")]
    Stale,
    #[error("records store failed")]
    Io,
    #[error("records backend unsupported")]
    Unsupported,
    #[error(transparent)]
    Codec(#[from] soil_sdk_codec::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<records::Error> for Error {
    fn from(error: records::Error) -> Self {
        match error {
            records::Error::Denied => Self::Denied,
            records::Error::NotFound => Self::NotFound,
            records::Error::OverBudget => Self::OverBudget,
            records::Error::Invalid => Self::Invalid,
            records::Error::Stale => Self::Stale,
            records::Error::Io => Self::Io,
            records::Error::Unsupported => Self::Unsupported,
        }
    }
}

pub fn key(collection: impl Into<String>, id: impl Into<String>) -> Key {
    Key {
        collection: collection.into(),
        id: id.into(),
    }
}

/// Untyped access to one collection.
pub struct Book {
    collection: String,
}

impl Book {
    pub fn new(collection: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
        }
    }

    pub fn collection(&self) -> &str {
        &self.collection
    }

    pub fn get(&self, id: impl Into<String>) -> Result<Option<Entry>> {
        match records::get(&key(self.collection.clone(), id)) {
            Ok(entry) => Ok(Some(entry)),
            Err(records::Error::NotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn scan(&self, cursor: Option<String>, limit: u32) -> Result<Page> {
        Ok(records::scan(&self.collection, cursor.as_deref(), limit)?)
    }

    pub fn upsert(
        &self,
        id: impl Into<String>,
        schema: impl Into<String>,
        body: Vec<u8>,
    ) -> Result<Entry> {
        Ok(records::upsert(&Entry {
            key: key(self.collection.clone(), id),
            schema: schema.into(),
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body,
        })?)
    }

    pub fn delete(&self, id: impl Into<String>) -> Result<()> {
        Ok(records::delete(&key(self.collection.clone(), id))?)
    }

    pub fn changes(&self, since: Option<String>, limit: u32) -> Result<Page> {
        Ok(records::changes(since.as_deref(), limit)?)
    }

    pub fn apply(&self, entries: Vec<Entry>) -> Result<records::ApplyResult> {
        Ok(records::apply(&entries)?)
    }
}

/// A body contract and collection name.
pub trait Record {
    const COLLECTION: &'static str;
    const SCHEMA: &'static str;
}

/// Typed access to one collection.
pub struct TypedBook<T> {
    book: Book,
    codec: JsonCodec,
    _value: PhantomData<T>,
}

impl<T: Record> TypedBook<T> {
    pub fn open() -> Self {
        Self {
            book: Book::new(T::COLLECTION),
            codec: JsonCodec,
            _value: PhantomData,
        }
    }

    pub fn get(&self, id: impl Into<String>) -> Result<Option<(T, Entry)>>
    where
        T: DeserializeOwned,
    {
        let Some(entry) = self.book.get(id)? else {
            return Ok(None);
        };
        let value = self.codec.decode(&entry.body)?;
        Ok(Some((value, entry)))
    }

    pub fn put(&self, id: impl Into<String>, value: &T) -> Result<Entry>
    where
        T: Serialize,
    {
        let body = self.codec.encode(value)?;
        self.book.upsert(id, T::SCHEMA, body)
    }

    pub fn delete(&self, id: impl Into<String>) -> Result<()> {
        self.book.delete(id)
    }

    pub fn scan(&self, cursor: Option<String>, limit: u32) -> Result<Vec<(T, Entry)>>
    where
        T: DeserializeOwned,
    {
        let page = self.book.scan(cursor, limit)?;
        let mut out = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            if entry.deleted {
                continue;
            }
            let value = self.codec.decode(&entry.body)?;
            out.push((value, entry));
        }
        Ok(out)
    }

    /// Materialized changes, preserving tombstone entries.
    pub fn changes(&self, since: Option<String>, limit: u32) -> Result<Vec<(Option<T>, Entry)>>
    where
        T: DeserializeOwned,
    {
        let page = self.book.changes(since, limit)?;
        let mut out = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let value = if entry.deleted {
                None
            } else {
                Some(self.codec.decode(&entry.body)?)
            };
            out.push((value, entry));
        }
        Ok(out)
    }

    /// Apply remote entries using the host's revision/origin merge.
    pub fn apply(&self, entries: Vec<Entry>) -> Result<records::ApplyResult> {
        self.book.apply(entries)
    }
}

#[cfg(test)]
mod tests {
    const WIT: &str = include_str!("../../../wit/soil-records.wit");

    #[test]
    fn pins_the_guest_world() {
        assert!(WIT.contains("package soil:records@0.1.0;"));
        assert!(WIT.contains("world guest"));
        assert!(WIT.contains("apply: func"));
    }
}
