//! Normalized conversations: threads, messages and attachment references.
//!
//! Message bodies never contain media bytes. An attachment names a
//! `ContentId`, display metadata, provider locators and an optional source ref.
//! VFS/IPFS/S3/local providers own the bytes.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use fungos_content::ContentId;
use fungos_records::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const THREAD_SCHEMA: &str = "fungos.conversation/thread@1";
pub const MESSAGE_SCHEMA: &str = "fungos.conversation/message@1";
pub const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRef {
    pub provider: String,
    pub locator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub media_type: String,
    pub size: u64,
    /// Canonical whole-content identity, e.g. `blake3:<hex>`.
    pub content_id: String,
    #[serde(default)]
    pub provider_refs: Vec<ProviderRef>,
    /// Matrix `mxc://`, URL, bridge id, or other provenance. Never authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<Box<Attachment>>,
}

impl Attachment {
    pub fn new(
        name: impl Into<String>,
        media_type: impl Into<String>,
        size: u64,
        content_id: impl Into<String>,
    ) -> Result<Self> {
        let content_id = content_id.into();
        ContentId::parse(&content_id)?;
        Ok(Self {
            name: name.into(),
            media_type: media_type.into(),
            size,
            content_id,
            provider_refs: Vec::new(),
            source_ref: None,
            width: None,
            height: None,
            duration_ms: None,
            thumbnail: None,
        })
    }

    pub fn add_provider(mut self, provider: impl Into<String>, locator: impl Into<String>) -> Self {
        let provider = provider.into();
        let locator = locator.into();
        if !self
            .provider_refs
            .iter()
            .any(|current| current.provider == provider && current.locator == locator)
        {
            self.provider_refs.push(ProviderRef { provider, locator });
        }
        self
    }

    pub fn with_source_ref(mut self, source_ref: impl Into<String>) -> Self {
        self.source_ref = Some(source_ref.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub channel: String,
    pub external_id: String,
    pub title: String,
    #[serde(default)]
    pub participants: Vec<String>,
    pub visibility: String,
    pub first_at_ms: u64,
    pub last_at_ms: u64,
}

impl Thread {
    pub fn record(&self) -> Value {
        json!({
            "channel": self.channel,
            "external_id": self.external_id,
            "title": self.title,
            "participants": self.participants,
            "visibility": self.visibility,
            "first_at_ms": self.first_at_ms,
            "last_at_ms": self.last_at_ms,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub thread_id: String,
    pub channel: String,
    pub external_id: String,
    pub direction: String,
    pub sender: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_principal: Option<String>,
    pub body: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    pub occurred_at_ms: u64,
    #[serde(default)]
    pub edited_at_ms: Option<u64>,
    #[serde(default)]
    pub deleted: bool,
}

impl Message {
    pub fn record(&self) -> Value {
        json!({
            "thread_id": self.thread_id,
            "channel": self.channel,
            "external_id": self.external_id,
            "direction": self.direction,
            "sender": self.sender,
            "sender_principal": self.sender_principal,
            "body": self.body,
            "attachments": self.attachments,
            "mentions": self.mentions,
            "source_refs": self.source_refs,
            "occurred_at_ms": self.occurred_at_ms,
            "edited_at_ms": self.edited_at_ms,
            "deleted": self.deleted,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() || self.thread_id.trim().is_empty() {
            anyhow::bail!("message id and thread id are required");
        }
        for attachment in &self.attachments {
            ContentId::parse(&attachment.content_id).with_context(|| {
                format!("message {} has invalid attachment content id", self.id)
            })?;
        }
        Ok(())
    }
}

pub fn put_thread(store: &mut Store, book: &str, thread: &Thread) -> Result<()> {
    store.put_record(
        book,
        &thread.id,
        THREAD_SCHEMA,
        SCHEMA_VERSION,
        thread.record(),
    )?;
    Ok(())
}

pub fn put_message(store: &mut Store, book: &str, message: &Message) -> Result<()> {
    message.validate()?;
    store.put_record(
        book,
        &message.id,
        MESSAGE_SCHEMA,
        SCHEMA_VERSION,
        message.record(),
    )?;
    Ok(())
}

/// In-memory derived index for tests and small hosts. A production host uses a
/// persistent index with the same `(occurred_at_ms, id)` cursor semantics.
#[derive(Default)]
pub struct ConversationIndex {
    threads: BTreeMap<String, Thread>,
    messages: BTreeMap<String, BTreeMap<(u64, String), Message>>,
}

impl ConversationIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_thread(&mut self, thread: Thread) {
        self.threads.insert(thread.id.clone(), thread);
    }

    pub fn upsert_message(&mut self, message: Message) {
        self.messages
            .entry(message.thread_id.clone())
            .or_default()
            .insert((message.occurred_at_ms, message.id.clone()), message);
    }

    pub fn thread(&self, id: &str) -> Option<&Thread> {
        self.threads.get(id)
    }

    /// Newest-first page. `before` is exclusive, so scrolling uses the last
    /// message's `(occurred_at_ms, id)` without an offset.
    pub fn messages(
        &self,
        thread_id: &str,
        before: Option<(u64, String)>,
        limit: usize,
    ) -> Vec<&Message> {
        let Some(messages) = self.messages.get(thread_id) else {
            return Vec::new();
        };
        messages
            .iter()
            .rev()
            .filter(|((at, id), _)| match &before {
                Some((before_at, before_id)) => (at, id) < (before_at, before_id),
                None => true,
            })
            .take(limit)
            .map(|(_, message)| message)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, at: u64) -> Message {
        Message {
            id: id.into(),
            thread_id: "wa:acme".into(),
            channel: "whatsapp".into(),
            external_id: format!("mx-{id}"),
            direction: "inbound".into(),
            sender: "Ada".into(),
            sender_principal: None,
            body: "hello".into(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            source_refs: vec![format!("mxc://matrix.example/{id}")],
            occurred_at_ms: at,
            edited_at_ms: None,
            deleted: false,
        }
    }

    #[test]
    fn media_is_a_content_reference_not_bytes() {
        let attachment = Attachment::new(
            "brief.pdf",
            "application/pdf",
            42,
            format!("blake3:{}", "a".repeat(64)),
        )
        .unwrap()
        .add_provider("ipfs", "bafy...")
        .add_provider("s3", "s3://bucket/brief.pdf")
        .with_source_ref("mxc://matrix.example/media");
        let value = serde_json::to_value(&attachment).unwrap();
        assert_eq!(value["content_id"], format!("blake3:{}", "a".repeat(64)));
        assert_eq!(value["provider_refs"][0]["provider"], "ipfs");
        assert!(value.get("bytes").is_none());
    }

    #[test]
    fn message_round_trips_through_records() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), &dir.path().join("device_id")).unwrap();
        let book = "personal/test/chat";
        let thread = Thread {
            id: "wa:acme".into(),
            channel: "whatsapp".into(),
            external_id: "!room:matrix".into(),
            title: "Acme".into(),
            participants: vec!["Ada".into()],
            visibility: "private".into(),
            first_at_ms: 1,
            last_at_ms: 2,
        };
        put_thread(&mut store, book, &thread).unwrap();
        put_message(&mut store, book, &message("m1", 1)).unwrap();
        let record = store.get(book, "m1").unwrap();
        assert_eq!(record.schema_id, MESSAGE_SCHEMA);
        assert_eq!(record.fields["thread_id"], "wa:acme");
    }

    #[test]
    fn cursor_pagination_is_stable() {
        let mut index = ConversationIndex::new();
        for at in 1..=5 {
            index.upsert_message(message(&format!("m{at}"), at));
        }
        let first = index.messages("wa:acme", None, 2);
        assert_eq!(
            first.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["m5", "m4"]
        );
        let cursor = (first[1].occurred_at_ms, first[1].id.clone());
        let second = index.messages("wa:acme", Some(cursor), 2);
        assert_eq!(
            second.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["m3", "m2"]
        );
    }
}
