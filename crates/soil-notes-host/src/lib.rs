//! Host-side notes root mapping and Markdown backlink population.
//!
//! The Notes Spore only names workspace-relative note paths. This host chooses
//! the note root, rejects escapes, and projects `[[wiki links]]` into the same
//! `fungos.graph/link` records the graph host already indexes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use fungos_records::Store;
use serde_json::json;

pub const LINK_SCHEMA: &str = "fungos.graph/link";
pub const LINK_VERSION: i64 = 1;
pub const LINK_REL: &str = "links";
pub const MAX_LINKS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteLink {
    pub source: String,
    pub target: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphLink {
    pub id: String,
    pub book: String,
    pub source: String,
    pub target: String,
    pub title: Option<String>,
}

/// A host-selected workspace subtree that contains Markdown notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotesRoot {
    root: String,
}

impl NotesRoot {
    pub fn new(root: impl Into<String>) -> Result<Self> {
        let root = normalize(&root.into())?;
        if root.is_empty() {
            bail!("notes root must not be empty");
        }
        if root.split('/').any(|part| part == ".." || part == ".") {
            bail!("notes root must not contain . or .. segments");
        }
        Ok(Self { root })
    }

    pub fn as_str(&self) -> &str {
        &self.root
    }

    /// Resolve a note path inside the configured root. Absolute paths and `..`
    /// escapes are refused, not silently clamped.
    pub fn resolve(&self, path: &str) -> Result<String> {
        let raw = path.trim();
        if raw.starts_with('/') {
            bail!("note path must be workspace-relative: {raw}");
        }
        let path = normalize(raw)?;
        if path.is_empty() {
            return Ok(self.root.clone());
        }
        if path == self.root || path.starts_with(&format!("{}/", self.root)) {
            return Ok(path);
        }
        let resolved = format!("{}/{}", self.root, path);
        normalize(&resolved)
    }

    /// The root-relative path, if `path` is inside this root.
    pub fn relative<'a>(&self, path: &'a str) -> Option<&'a str> {
        if path == self.root {
            return Some("");
        }
        path.strip_prefix(&format!("{}/", self.root))
    }

    pub fn is_note(&self, path: &str) -> bool {
        self.resolve(path).is_ok_and(|path| is_markdown(&path))
    }
}

/// Parse `[[target]]` and `[[target|label]]` wiki links from one note. Targets
/// are resolved against the notes root, with a `.md` suffix added when absent.
pub fn parse_links(root: &NotesRoot, source: &str, markdown: &str) -> Result<Vec<NoteLink>> {
    let source = root.resolve(source)?;
    if !is_markdown(&source) {
        bail!("source is not a Markdown note: {source}");
    }
    let mut links = Vec::new();
    let mut seen = BTreeSet::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("[[") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("]]") else { break };
        let inner = &rest[..end];
        rest = &rest[end + 2..];
        let (target, label) = match inner.split_once('|') {
            Some((target, label)) => (target.trim(), Some(label.trim())),
            None => (inner.trim(), None),
        };
        if target.is_empty()
            || target.starts_with('#')
            || target.contains("://")
            || target.len() > 512
        {
            continue;
        }
        let target = target.split('#').next().unwrap_or(target).trim();
        if target.is_empty() {
            continue;
        }
        let target = with_markdown_suffix(&root.resolve(target)?);
        if target == source || !seen.insert((target.clone(), label.map(str::to_owned))) {
            continue;
        }
        links.push(NoteLink {
            source: source.clone(),
            target,
            label: label.filter(|label| !label.is_empty()).map(str::to_owned),
        });
        if links.len() >= MAX_LINKS {
            break;
        }
    }
    Ok(links)
}

/// Reconcile one note's `links` graph records with its current Markdown.
pub fn sync_note_links(
    store: &mut Store,
    book: &str,
    root: &NotesRoot,
    source: &str,
    markdown: &str,
) -> Result<Vec<String>> {
    let source_path = root.resolve(source)?;
    let source_locator = note_locator(&source_path);
    let desired = parse_links(root, source, markdown)?
        .into_iter()
        .map(|link| {
            (
                link_id(&source_path, &link.target),
                (note_locator(&link.target), link.label),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let existing = store
        .list_schema(book, LINK_SCHEMA, 10_000)
        .with_context(|| format!("read note links in {book}"))?;
    for record in existing {
        if record.fields["from"] != source_locator || record.fields["rel"] != LINK_REL {
            continue;
        }
        if !desired.contains_key(&record.entity_id) {
            store.delete_record(book, &record.entity_id, LINK_SCHEMA, LINK_VERSION)?;
        }
    }

    for (id, (target, label)) in &desired {
        let mut fields = json!({
            "from": source_locator,
            "rel": LINK_REL,
            "to": target,
        });
        if let Some(label) = label {
            fields["title"] = json!(label);
        }
        store.put_record(book, id, LINK_SCHEMA, LINK_VERSION, fields)?;
    }
    Ok(desired.keys().cloned().collect())
}

/// Incoming note links for one note path across approved books.
pub fn note_backlinks(
    store: &Store,
    books: &[String],
    root: &NotesRoot,
    path: &str,
) -> Result<Vec<GraphLink>> {
    let target = note_locator(&root.resolve(path)?);
    let mut links = Vec::new();
    for book in books {
        let records = store
            .list_schema(book, LINK_SCHEMA, 10_000)
            .with_context(|| format!("read note links in {book}"))?;
        for record in records {
            if record.fields["rel"] != LINK_REL || record.fields["to"] != target {
                continue;
            }
            links.push(GraphLink {
                id: record.entity_id,
                book: record.book_id,
                source: record.fields["from"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                target: target.clone(),
                title: record.fields["title"].as_str().map(str::to_owned),
            });
        }
    }
    links.sort_by(|left, right| {
        left.book
            .cmp(&right.book)
            .then_with(|| left.source.cmp(&right.source))
    });
    Ok(links)
}

/// Remove every graph link owned by one note.
pub fn clear_note_links(
    store: &mut Store,
    book: &str,
    root: &NotesRoot,
    source: &str,
) -> Result<()> {
    let source = note_locator(&root.resolve(source)?);
    let existing = store
        .list_schema(book, LINK_SCHEMA, 10_000)
        .with_context(|| format!("read note links in {book}"))?;
    for record in existing {
        if record.fields["from"] == source && record.fields["rel"] == LINK_REL {
            store.delete_record(book, &record.entity_id, LINK_SCHEMA, LINK_VERSION)?;
        }
    }
    Ok(())
}

pub fn note_locator(path: &str) -> String {
    format!("vfs:{}", path.trim_start_matches('/'))
}

fn normalize(path: &str) -> Result<String> {
    let path = path.trim();
    if path.contains('\0') {
        bail!("note path contains NUL");
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("note path escapes the workspace root: {path}");
                }
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

fn is_markdown(path: &str) -> bool {
    matches!(
        path.rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "md" | "markdown" | "mdx"
    )
}

fn with_markdown_suffix(path: &str) -> String {
    if is_markdown(path) {
        path.to_string()
    } else {
        format!("{path}.md")
    }
}

fn link_id(source: &str, target: &str) -> String {
    let digest = blake3::hash(format!("{source}\0{target}").as_bytes());
    format!("note-link-{}", &digest.to_hex()[..20])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> NotesRoot {
        NotesRoot::new("notes").unwrap()
    }

    #[test]
    fn root_mapping_refuses_escapes_and_normalizes() {
        let root = root();
        assert_eq!(root.resolve("welcome.md").unwrap(), "notes/welcome.md");
        assert!(root.resolve("/notes/welcome.md").is_err());
        assert_eq!(root.resolve("notes/a/../b.md").unwrap(), "notes/b.md");
        assert!(root.resolve("../secrets.md").is_err());
        assert!(root.resolve("/etc/passwd").is_err());
        assert_eq!(root.relative("notes/a/b.md"), Some("a/b.md"));
        assert_eq!(root.relative("other/a.md"), None);
    }

    #[test]
    fn links_are_root_relative_labeled_and_markdown_suffixed() {
        let links = parse_links(
            &root(),
            "projects/soil.md",
            "See [[welcome]], [[reference/dcp|DCP]] and [[welcome#top]].",
        )
        .unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "notes/welcome.md");
        assert_eq!(links[1].target, "notes/reference/dcp.md");
        assert_eq!(links[1].label.as_deref(), Some("DCP"));
    }

    #[test]
    fn graph_records_round_trip_backlinks_and_drop_stale_links() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), &dir.path().join("device_id")).unwrap();
        let book = "personal/avery/notes";
        let root = root();
        sync_note_links(
            &mut store,
            book,
            &root,
            "welcome.md",
            "See [[projects/soil]] and [[reference/dcp]].",
        )
        .unwrap();
        let backlinks = note_backlinks(&store, &[book.into()], &root, "projects/soil.md").unwrap();
        assert_eq!(backlinks.len(), 1);
        assert_eq!(backlinks[0].source, "vfs:notes/welcome.md");

        sync_note_links(
            &mut store,
            book,
            &root,
            "welcome.md",
            "See [[reference/dcp]].",
        )
        .unwrap();
        assert!(
            note_backlinks(&store, &[book.into()], &root, "projects/soil.md")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            note_backlinks(&store, &[book.into()], &root, "reference/dcp.md")
                .unwrap()
                .len(),
            1
        );
    }
}
