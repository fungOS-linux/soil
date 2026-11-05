//! Mention tokens and their graph links.
//!
//! A mention is a stable locator embedded in human text:
//!
//! ```text
//! @[Ada Lovelace](record:personal/avery/crm#c-ada)
//! @[Budget](file:sheets/budget.xlsx)
//! ```
//!
//! The label is a display fallback. Identity is the locator. Persistence writes
//! one `fungos.graph/link` record per source/target pair so the existing graph
//! index can answer backlinks without a second database.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use fungos_records::Store;
use serde_json::json;

pub const LINK_SCHEMA: &str = "fungos.graph/link";
pub const LINK_VERSION: i64 = 1;
pub const MENTION_REL: &str = "mentions";
pub const MAX_MENTIONS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub label: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionLink {
    pub id: String,
    pub book: String,
    pub source: String,
    pub target: String,
    pub title: Option<String>,
}

/// Parse `@[label](scheme:value)` tokens in source order.
pub fn parse(body: &str) -> Vec<Mention> {
    let mut mentions = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = body[cursor..].find("@[") {
        let start = cursor + relative;
        let label_start = start + 2;
        let Some(label_end_relative) = body[label_start..].find("](") else {
            break;
        };
        let label_end = label_start + label_end_relative;
        let target_start = label_end + 2;
        let Some(target_end_relative) = body[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + target_end_relative;
        let label = body[label_start..label_end].trim();
        let target = body[target_start..target_end].trim();
        if !label.is_empty() && is_locator(target) {
            mentions.push(Mention {
                label: label.to_string(),
                target: target.to_string(),
            });
        }
        cursor = target_end + 1;
        if mentions.len() >= MAX_MENTIONS {
            break;
        }
    }
    mentions
}

/// Qualified locator for a record source.
pub fn source_locator(book: &str, source_id: &str) -> String {
    format!("record:{book}#{source_id}")
}

/// Reconcile the source's `mentions` graph links with the links in `body`.
///
/// Returns the stable link ids that are now present.
pub fn sync(store: &mut Store, book: &str, source_id: &str, body: &str) -> Result<Vec<String>> {
    let source = source_locator(book, source_id);
    let desired = desired_links(book, source_id, body);
    let existing = store
        .list_schema(book, LINK_SCHEMA, 10_000)
        .with_context(|| format!("read mention links in {book}"))?;

    let mut present = BTreeSet::new();
    for record in existing {
        if record.fields["from"] != source || record.fields["rel"] != MENTION_REL {
            continue;
        }
        present.insert(record.entity_id.clone());
        if !desired.contains_key(&record.entity_id) {
            store.delete_record(book, &record.entity_id, LINK_SCHEMA, LINK_VERSION)?;
        }
    }

    for (id, (target, label)) in &desired {
        store.put_record(
            book,
            id,
            LINK_SCHEMA,
            LINK_VERSION,
            json!({
                "from": source,
                "rel": MENTION_REL,
                "to": target,
                "title": label,
            }),
        )?;
    }

    Ok(desired.keys().cloned().collect())
}

/// Remove every mention link owned by one source record.
pub fn clear(store: &mut Store, book: &str, source_id: &str) -> Result<()> {
    let source = source_locator(book, source_id);
    let existing = store
        .list_schema(book, LINK_SCHEMA, 10_000)
        .with_context(|| format!("read mention links in {book}"))?;
    for record in existing {
        if record.fields["from"] == source && record.fields["rel"] == MENTION_REL {
            store.delete_record(book, &record.entity_id, LINK_SCHEMA, LINK_VERSION)?;
        }
    }
    Ok(())
}

/// Outgoing mention links for one source record.
pub fn links_for_source(store: &Store, book: &str, source_id: &str) -> Result<Vec<MentionLink>> {
    let source = source_locator(book, source_id);
    let records = store
        .list_schema(book, LINK_SCHEMA, 10_000)
        .with_context(|| format!("read mention links in {book}"))?;
    Ok(records
        .into_iter()
        .filter(|record| record.fields["from"] == source && record.fields["rel"] == MENTION_REL)
        .map(|record| MentionLink {
            id: record.entity_id,
            book: record.book_id,
            source: record.fields["from"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            target: record.fields["to"].as_str().unwrap_or_default().to_string(),
            title: record.fields["title"].as_str().map(str::to_string),
        })
        .collect())
}

/// Incoming mention links for one target locator across approved books.
pub fn backlinks(store: &Store, books: &[String], target: &str) -> Result<Vec<MentionLink>> {
    let mut links = Vec::new();
    for book in books {
        let records = store
            .list_schema(book, LINK_SCHEMA, 10_000)
            .with_context(|| format!("read mention links in {book}"))?;
        for record in records {
            if record.fields["rel"] != MENTION_REL {
                continue;
            }
            let Some(to) = record.fields["to"].as_str() else {
                continue;
            };
            if !target_matches(target, to) {
                continue;
            }
            links.push(MentionLink {
                id: record.entity_id,
                book: record.book_id,
                source: record.fields["from"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                target: to.to_string(),
                title: record.fields["title"].as_str().map(str::to_string),
            });
        }
    }
    links.sort_by(|left, right| {
        left.book
            .cmp(&right.book)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.target.cmp(&right.target))
    });
    Ok(links)
}

fn desired_links(book: &str, source_id: &str, body: &str) -> BTreeMap<String, (String, String)> {
    let mut desired = BTreeMap::new();
    for mention in parse(body) {
        let id = link_id(book, source_id, &mention.target);
        desired.entry(id).or_insert((mention.target, mention.label));
    }
    desired
}

fn link_id(book: &str, source_id: &str, target: &str) -> String {
    let digest = blake3::hash(format!("{book}\0{source_id}\0{target}").as_bytes());
    let hex = digest.to_hex();
    format!("mention-{source_id}-{}", &hex[..16])
}

fn is_locator(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
        && scheme
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
}

fn target_matches(query: &str, stored: &str) -> bool {
    if query == stored {
        return true;
    }
    if let Some((book, entity)) = query
        .strip_prefix("record:")
        .and_then(|rest| rest.split_once('#'))
    {
        return stored == entity || stored == format!("{book}/{entity}");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &std::path::Path) -> Store {
        Store::open(root, &root.join("device_id")).unwrap()
    }

    #[test]
    fn parses_tokens_and_ignores_invalid_ones() {
        let body = "Talk to @[Ada](record:personal/crm#c-ada) about \
                    @[Budget](file:sheets/budget.xlsx), not @[bad](nope) or @[x]().";
        let mentions = parse(body);
        assert_eq!(mentions.len(), 2);
        assert_eq!(mentions[0].label, "Ada");
        assert_eq!(mentions[0].target, "record:personal/crm#c-ada");
        assert_eq!(mentions[1].target, "file:sheets/budget.xlsx");
    }

    #[test]
    fn sync_writes_updates_and_removes_links() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let book = "personal/test/tasks";
        let body = "@[Ada](record:personal/crm#c-ada) and \
                    @[Ada again](record:personal/crm#c-ada)";
        let ids = sync(&mut store, book, "task-1", body).unwrap();
        assert_eq!(ids.len(), 1, "one link per source/target pair");

        let links = links_for_source(&store, book, "task-1").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].source, source_locator(book, "task-1"));
        assert_eq!(links[0].target, "record:personal/crm#c-ada");

        sync(
            &mut store,
            book,
            "task-1",
            "@[Budget](file:sheets/budget.xlsx)",
        )
        .unwrap();
        let links = links_for_source(&store, book, "task-1").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "file:sheets/budget.xlsx");

        sync(&mut store, book, "task-1", "plain text").unwrap();
        assert!(links_for_source(&store, book, "task-1").unwrap().is_empty());
    }

    #[test]
    fn backlinks_find_incoming_mentions() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let books = vec![
            "personal/test/tasks".to_string(),
            "personal/test/docs".to_string(),
        ];
        sync(
            &mut store,
            &books[0],
            "task-1",
            "@[Ada](record:personal/crm#c-ada)",
        )
        .unwrap();
        sync(
            &mut store,
            &books[1],
            "doc-1",
            "@[Ada](record:personal/crm#c-ada)",
        )
        .unwrap();

        let links = backlinks(&store, &books, "record:personal/crm#c-ada").unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].book, "personal/test/docs");
        assert_eq!(links[1].book, "personal/test/tasks");
    }
}
