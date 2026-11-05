//! Persistent, derived index for `soil:index`.
//!
//! SQLite owns entries, extracted text, FTS5 chunks and an optional vector
//! cache. It is a cache: the VFS namespace and `records` remain the sources of
//! truth. Deleting this database must never lose user data.

use std::{
    collections::BTreeMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use fungos_vfs::{FileEntry, Hydration, Vfs};
use rusqlite::{Connection, OptionalExtension, params};

use crate::soil::index::index;

pub(crate) const MAX_INDEX_TEXT: usize = 64 * 1024;
const MAX_CHUNK_BYTES: usize = 4096;
const MAX_FULL_TEXT_ROWS: usize = 4096;

/// Replaceable retrieval backend. SQLite + FTS5 is the first implementation;
/// a dedicated ANN store can replace it without changing `soil:index`.
pub(crate) trait IndexBackend: Send {
    fn refresh(&mut self, vfs: &mut Vfs) -> anyhow::Result<()>;
    fn state(&self) -> index::IndexState;
    fn browse(
        &self,
        parent: &str,
        order: index::Order,
        offset: u64,
        limit: u32,
    ) -> Result<index::Page, index::Error>;
    fn search(&self, query: &index::Query) -> Result<index::Page, index::Error>;
    fn entry(&self, path: &str) -> Option<&Node>;
    fn nodes(&self) -> &BTreeMap<String, Node>;
}

#[derive(Clone)]
pub(crate) struct Node {
    pub(crate) entry: FileEntry,
    pub(crate) kind: NodeKind,
    pub(crate) child_count: u64,
    pub(crate) text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeKind {
    File,
    Directory,
}

pub(crate) struct SqliteIndex {
    db: Connection,
    nodes: BTreeMap<String, Node>,
    generation: u64,
    rebuilt_at_ms: u64,
    ready: bool,
}

impl SqliteIndex {
    pub(crate) fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db =
            Connection::open(path).with_context(|| format!("open index {}", path.display()))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS meta(
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS entries(
               path TEXT PRIMARY KEY,
               content_id TEXT NOT NULL,
               size INTEGER NOT NULL,
               media_type TEXT NOT NULL,
               version INTEGER NOT NULL,
               hydration TEXT NOT NULL,
               pinned INTEGER NOT NULL,
               updated_at_ms INTEGER NOT NULL,
               kind TEXT NOT NULL,
               child_count INTEGER NOT NULL,
               text TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS chunks(
               chunk_id TEXT PRIMARY KEY,
               path TEXT NOT NULL,
               content_id TEXT NOT NULL,
               ordinal INTEGER NOT NULL,
               text TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);
             CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
               chunk_id UNINDEXED,
               path UNINDEXED,
               text,
               tokenize='unicode61'
             );
             CREATE TABLE IF NOT EXISTS vectors(
               chunk_id TEXT NOT NULL,
               model TEXT NOT NULL,
               dimension INTEGER NOT NULL,
               vector BLOB NOT NULL,
               norm REAL NOT NULL,
               PRIMARY KEY(chunk_id, model)
             );",
        )?;
        let mut index = Self {
            db,
            nodes: BTreeMap::new(),
            generation: 0,
            rebuilt_at_ms: 0,
            ready: false,
        };
        index.load()?;
        Ok(index)
    }

    fn load(&mut self) -> anyhow::Result<()> {
        self.generation = self
            .meta("generation")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        self.rebuilt_at_ms = self
            .meta("rebuilt_at_ms")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        self.ready = self.meta("ready")?.as_deref() == Some("1");

        let mut statement = self.db.prepare(
            "SELECT path,content_id,size,media_type,version,hydration,pinned,updated_at_ms,kind,child_count,text
             FROM entries ORDER BY path",
        )?;
        let rows = statement.query_map([], |row| {
            let path: String = row.get(0)?;
            let kind: String = row.get(8)?;
            Ok((
                path.clone(),
                Node {
                    entry: FileEntry {
                        path,
                        content_id: fungos_vfs::ContentId(row.get(1)?),
                        size: row.get::<_, i64>(2)?.max(0) as u64,
                        media_type: row.get(3)?,
                        version: row.get::<_, i64>(4)?.max(0) as u64,
                        hydration: if row.get::<_, String>(5)? == "placeholder" {
                            Hydration::Placeholder
                        } else {
                            Hydration::Present
                        },
                        pinned: row.get::<_, i64>(6)? != 0,
                        updated_at_ms: row.get(7)?,
                    },
                    kind: if kind == "dir" {
                        NodeKind::Directory
                    } else {
                        NodeKind::File
                    },
                    child_count: row.get::<_, i64>(9)?.max(0) as u64,
                    text: row.get(10)?,
                },
            ))
        })?;
        for row in rows {
            let (path, node) = row?;
            self.nodes.insert(path, node);
        }
        Ok(())
    }

    fn meta(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM meta WHERE key=?1", params![key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.db.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn persisted_source(&self) -> anyhow::Result<Option<String>> {
        self.meta("source_hash")
    }

    /// Persist one vector for a current chunk. Vectors are derived cache data,
    /// never authority and never synced by `records`.
    #[allow(dead_code)]
    pub(crate) fn put_vector(
        &mut self,
        chunk_id: &str,
        model: &str,
        vector: &[f32],
    ) -> anyhow::Result<()> {
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        let mut norm = 0.0f32;
        for value in vector {
            norm += value * value;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let norm = norm.sqrt();
        self.db.execute(
            "INSERT INTO vectors(chunk_id,model,dimension,vector,norm) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(chunk_id,model) DO UPDATE SET
               dimension=excluded.dimension,
               vector=excluded.vector,
               norm=excluded.norm",
            params![chunk_id, model, vector.len() as i64, bytes, norm as f64],
        )?;
        Ok(())
    }

    /// Brute-force cosine search. This is the v1 baseline; `sqlite-vec`, HNSW
    /// or LanceDB can be added behind [`IndexBackend`] when scale requires it.
    #[allow(dead_code)]
    pub(crate) fn vector_search(
        &self,
        model: &str,
        query: &[f32],
        limit: usize,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        let query_norm = query.iter().map(|value| value * value).sum::<f32>().sqrt();
        if query_norm == 0.0 || limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.db.prepare(
            "SELECT vectors.chunk_id,vectors.dimension,vectors.vector,vectors.norm
             FROM vectors JOIN chunks ON chunks.chunk_id=vectors.chunk_id
             WHERE vectors.model=?1",
        )?;
        let rows = statement.query_map(params![model], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as usize,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, f64>(3)? as f32,
            ))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (chunk_id, dimension, bytes, norm) = row?;
            if dimension != query.len() || bytes.len() != dimension * 4 {
                continue;
            }
            let mut dot = 0.0f32;
            for (index, value) in query.iter().enumerate() {
                let start = index * 4;
                let stored = f32::from_le_bytes([
                    bytes[start],
                    bytes[start + 1],
                    bytes[start + 2],
                    bytes[start + 3],
                ]);
                dot += value * stored;
            }
            if norm > 0.0 {
                hits.push((chunk_id, dot / (query_norm * norm)));
            }
        }
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.truncate(limit);
        Ok(hits)
    }

    fn rebuild(
        &mut self,
        vfs: &mut Vfs,
        entries: &[FileEntry],
        source_hash: &str,
    ) -> anyhow::Result<()> {
        let nodes = build_nodes(vfs, entries);
        let generation = self.generation.saturating_add(1);
        let rebuilt_at_ms = now_ms();
        let transaction = self.db.transaction()?;
        transaction.execute("DELETE FROM chunks_fts", [])?;
        transaction.execute("DELETE FROM chunks", [])?;
        transaction.execute("DELETE FROM vectors", [])?;
        transaction.execute("DELETE FROM entries", [])?;
        for node in nodes.values() {
            transaction.execute(
                "INSERT INTO entries(
                   path,content_id,size,media_type,version,hydration,pinned,updated_at_ms,kind,child_count,text
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    node.entry.path,
                    node.entry.content_id.to_string(),
                    node.entry.size as i64,
                    node.entry.media_type,
                    node.entry.version as i64,
                    if node.entry.hydration == Hydration::Placeholder {
                        "placeholder"
                    } else {
                        "present"
                    },
                    node.entry.pinned as i64,
                    node.entry.updated_at_ms,
                    if node.kind == NodeKind::Directory { "dir" } else { "file" },
                    node.child_count as i64,
                    node.text,
                ],
            )?;
            if node.kind == NodeKind::File && !node.text.is_empty() {
                let content_id = node.entry.content_id.to_string();
                for (ordinal, (start, end)) in chunk_ranges(&node.text).into_iter().enumerate() {
                    let chunk_id = format!("{content_id}:{ordinal}");
                    let text = &node.text[start..end];
                    transaction.execute(
                        "INSERT OR REPLACE INTO chunks(chunk_id,path,content_id,ordinal,text)
                         VALUES(?1,?2,?3,?4,?5)",
                        params![chunk_id, node.entry.path, content_id, ordinal as i64, text],
                    )?;
                    transaction.execute(
                        "INSERT INTO chunks_fts(chunk_id,path,text) VALUES(?1,?2,?3)",
                        params![chunk_id, node.entry.path, text],
                    )?;
                }
            }
        }
        transaction.commit()?;
        self.nodes = nodes;
        self.generation = generation;
        self.rebuilt_at_ms = rebuilt_at_ms;
        self.ready = true;
        self.set_meta("generation", &generation.to_string())?;
        self.set_meta("rebuilt_at_ms", &rebuilt_at_ms.to_string())?;
        self.set_meta("ready", "1")?;
        self.set_meta("source_hash", source_hash)?;
        Ok(())
    }

    fn memory_search(
        &self,
        query: &index::Query,
        parent: Option<&str>,
    ) -> Result<index::Page, index::Error> {
        let needle = query.text.to_lowercase();
        let mode = query.mode.unwrap_or(index::Mode::Substring);
        let mut hits = Vec::new();
        for node in self.nodes.values() {
            if node.kind == NodeKind::Directory {
                continue;
            }
            if !matches_filters(node, parent, query.media_type_prefix.as_deref()) {
                continue;
            }
            let path = node.entry.path.to_lowercase();
            let text = node.text.to_lowercase();
            let Some((score, matched_by)) = (match mode {
                index::Mode::Substring => path
                    .contains(&needle)
                    .then_some((1, index::MatchSource::Path))
                    .or_else(|| {
                        text.contains(&needle)
                            .then_some((2, index::MatchSource::Text))
                    }),
                index::Mode::Fuzzy => {
                    fuzzy_score(&path, &needle).map(|score| (score, index::MatchSource::Path))
                }
                index::Mode::FullText => path
                    .contains(&needle)
                    .then_some((1, index::MatchSource::Path))
                    .or_else(|| {
                        text.contains(&needle)
                            .then_some((2, index::MatchSource::Text))
                    }),
                index::Mode::Semantic => unreachable!(),
            }) else {
                continue;
            };
            hits.push(index::Hit {
                path: node.entry.path.clone(),
                score,
                snippet: text_snippet(&text, &needle),
                matched_by: Some(matched_by),
            });
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
        let start = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let page: Vec<_> = hits
            .into_iter()
            .skip(start)
            .take(query.limit as usize)
            .collect();
        Ok(index::Page {
            hits: page,
            next: None,
            generation: self.generation,
        })
    }

    fn full_text_search(
        &self,
        query: &index::Query,
        parent: Option<&str>,
    ) -> Result<index::Page, index::Error> {
        let Some(fts_query) = fts_query(&query.text) else {
            return Err(index::Error::Invalid);
        };
        let needle = query.text.to_lowercase();
        let mut best: BTreeMap<String, (f64, Option<String>)> = BTreeMap::new();
        let mut statement = self
            .db
            .prepare(
                "SELECT chunks_fts.path,bm25(chunks_fts),snippet(chunks_fts,2,'','','…',16)
                 FROM chunks_fts
                 WHERE chunks_fts MATCH ?1
                 ORDER BY bm25(chunks_fts)
                 LIMIT ?2",
            )
            .map_err(|_| index::Error::Io)?;
        let rows = statement
            .query_map(params![fts_query, MAX_FULL_TEXT_ROWS as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| index::Error::Io)?;
        for row in rows {
            let (path, rank, snippet) = row.map_err(|_| index::Error::Io)?;
            let Some(node) = self.nodes.get(&path) else {
                continue;
            };
            if !matches_filters(node, parent, query.media_type_prefix.as_deref()) {
                continue;
            }
            best.entry(path)
                .and_modify(|entry| {
                    if rank < entry.0 {
                        *entry = (rank, Some(snippet.clone()));
                    }
                })
                .or_insert((rank, Some(snippet)));
        }

        let mut ranked: Vec<_> = best.into_iter().collect();
        ranked.sort_by(|a, b| a.1.0.total_cmp(&b.1.0).then_with(|| a.0.cmp(&b.0)));
        let mut hits: Vec<index::Hit> = ranked
            .into_iter()
            .enumerate()
            .map(|(index, (path, (_, snippet)))| index::Hit {
                path,
                score: (MAX_FULL_TEXT_ROWS - index).max(1) as u32,
                snippet,
                matched_by: Some(index::MatchSource::Text),
            })
            .collect();

        // Spotlight-style recall: an exact path hit should not be buried by
        // body text ranking.
        for node in self.nodes.values() {
            if node.kind == NodeKind::Directory
                || !matches_filters(node, parent, query.media_type_prefix.as_deref())
            {
                continue;
            }
            if node.entry.path.to_lowercase().contains(&needle)
                && let Some(hit) = hits.iter_mut().find(|hit| hit.path == node.entry.path)
            {
                hit.score = MAX_FULL_TEXT_ROWS as u32 + 1;
                hit.matched_by = Some(index::MatchSource::Path);
            } else if node.entry.path.to_lowercase().contains(&needle) {
                hits.push(index::Hit {
                    path: node.entry.path.clone(),
                    score: MAX_FULL_TEXT_ROWS as u32 + 1,
                    snippet: None,
                    matched_by: Some(index::MatchSource::Path),
                });
            }
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
        let start = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let page: Vec<_> = hits
            .into_iter()
            .skip(start)
            .take(query.limit as usize)
            .collect();
        Ok(index::Page {
            hits: page,
            next: None,
            generation: self.generation,
        })
    }
}

impl IndexBackend for SqliteIndex {
    fn refresh(&mut self, vfs: &mut Vfs) -> anyhow::Result<()> {
        let entries = vfs.list("")?;
        let source_hash = source_hash(&entries);
        if self.ready && self.persisted_source()?.as_deref() == Some(&source_hash) {
            return Ok(());
        }
        self.rebuild(vfs, &entries, &source_hash)
    }

    fn state(&self) -> index::IndexState {
        index::IndexState {
            ready: self.ready,
            generation: self.generation,
            entries: self.nodes.len() as u64,
            rebuilt_at_ms: self.rebuilt_at_ms,
        }
    }

    fn browse(
        &self,
        parent: &str,
        order: index::Order,
        offset: u64,
        limit: u32,
    ) -> Result<index::Page, index::Error> {
        if !self.ready {
            return Err(index::Error::NotReady);
        }
        let Some(parent_node) = self.entry(parent) else {
            return Err(index::Error::Invalid);
        };
        if parent_node.kind != NodeKind::Directory {
            return Err(index::Error::Invalid);
        }
        let mut nodes: Vec<_> = self
            .nodes
            .values()
            .filter(|node| parent_of(&node.entry.path) == parent)
            .collect();
        match order {
            index::Order::Path => nodes.sort_by(|a, b| a.entry.path.cmp(&b.entry.path)),
            index::Order::PathDesc => nodes.sort_by(|a, b| b.entry.path.cmp(&a.entry.path)),
            index::Order::Modified => {
                nodes.sort_by_key(|node| (node.entry.updated_at_ms, node.entry.path.clone()))
            }
            index::Order::ModifiedDesc => {
                nodes.sort_by_key(|node| (-node.entry.updated_at_ms, node.entry.path.clone()))
            }
            index::Order::Size => {
                nodes.sort_by_key(|node| (node.entry.size, node.entry.path.clone()))
            }
            index::Order::SizeDesc => nodes
                .sort_by_key(|node| (std::cmp::Reverse(node.entry.size), node.entry.path.clone())),
        }
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let total = nodes.len() as u64;
        let hits: Vec<_> = nodes
            .into_iter()
            .skip(start)
            .take(limit as usize)
            .map(|node| index::Hit {
                path: node.entry.path.clone(),
                score: 0,
                snippet: None,
                matched_by: None,
            })
            .collect();
        let consumed = offset.saturating_add(hits.len() as u64);
        Ok(index::Page {
            hits,
            next: (consumed < total).then_some(consumed),
            generation: self.generation,
        })
    }

    fn search(&self, query: &index::Query) -> Result<index::Page, index::Error> {
        if query.limit == 0 || query.limit > crate::MAX_PAGE {
            return Err(index::Error::OverBudget);
        }
        if query.text.trim().is_empty() {
            return Err(index::Error::Invalid);
        }
        let parent = query
            .parent
            .as_deref()
            .map(crate::normalize_parent)
            .transpose()
            .map_err(|_| index::Error::Invalid)?;
        if !self.ready {
            return Err(index::Error::NotReady);
        }
        match query.mode.unwrap_or(index::Mode::Substring) {
            index::Mode::Semantic => Err(index::Error::Unsupported),
            index::Mode::FullText => self.full_text_search(query, parent.as_deref()),
            mode => {
                let mut query = query.clone();
                query.mode = Some(mode);
                self.memory_search(&query, parent.as_deref())
            }
        }
    }

    fn entry(&self, path: &str) -> Option<&Node> {
        self.nodes.get(path)
    }

    fn nodes(&self) -> &BTreeMap<String, Node> {
        &self.nodes
    }
}

fn build_nodes(vfs: &mut Vfs, entries: &[FileEntry]) -> BTreeMap<String, Node> {
    let mut nodes = BTreeMap::new();
    nodes.insert(
        String::new(),
        Node {
            entry: FileEntry {
                path: String::new(),
                content_id: fungos_vfs::ContentId(String::new()),
                size: 0,
                media_type: String::new(),
                version: 0,
                hydration: Hydration::Present,
                pinned: false,
                updated_at_ms: 0,
            },
            kind: NodeKind::Directory,
            child_count: 0,
            text: String::new(),
        },
    );

    for entry in entries {
        let path = entry.path.clone();
        add_ancestors(&mut nodes, &path);
        let text = if entry.media_type.starts_with("text/")
            && entry.hydration == Hydration::Present
            && entry.size <= MAX_INDEX_TEXT as u64
        {
            vfs.get(&path)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        nodes.insert(
            path.clone(),
            Node {
                entry: entry.clone(),
                kind: NodeKind::File,
                child_count: 0,
                text,
            },
        );
    }

    let paths: Vec<String> = nodes.keys().cloned().collect();
    for path in paths {
        if path.is_empty() {
            continue;
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            if let Some(node) = nodes.get_mut(parent) {
                node.child_count += 1;
            }
        } else if let Some(root) = nodes.get_mut("") {
            root.child_count += 1;
        }
    }
    nodes
}

fn add_ancestors(nodes: &mut BTreeMap<String, Node>, path: &str) {
    let mut prefix = String::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        nodes.entry(prefix.clone()).or_insert_with(|| Node {
            entry: FileEntry {
                path: prefix.clone(),
                content_id: fungos_vfs::ContentId(String::new()),
                size: 0,
                media_type: String::new(),
                version: 0,
                hydration: Hydration::Present,
                pinned: false,
                updated_at_ms: 0,
            },
            kind: NodeKind::Directory,
            child_count: 0,
            text: String::new(),
        });
    }
}

fn source_hash(entries: &[FileEntry]) -> String {
    let mut hasher = blake3::Hasher::new();
    for entry in entries {
        hasher.update(entry.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.content_id.0.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.media_type.as_bytes());
        hasher.update(b"\0");
        hasher.update(&entry.size.to_le_bytes());
        hasher.update(&entry.version.to_le_bytes());
        hasher.update(&entry.updated_at_ms.to_le_bytes());
        hasher.update(&[matches!(entry.hydration, Hydration::Placeholder) as u8]);
        hasher.update(&[entry.pinned as u8]);
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

fn chunk_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut last = 0usize;
    for (index, ch) in text.char_indices() {
        let end = index + ch.len_utf8();
        if end - start > MAX_CHUNK_BYTES && index > start {
            ranges.push((start, last));
            start = index;
        }
        last = end;
    }
    if start < text.len() {
        ranges.push((start, text.len()));
    }
    ranges
}

fn fts_query(text: &str) -> Option<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if tokens.is_empty() {
        return None;
    }
    Some(
        tokens
            .into_iter()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

fn matches_filters(node: &Node, parent: Option<&str>, media_type_prefix: Option<&str>) -> bool {
    if let Some(parent) = parent
        && parent_of(&node.entry.path) != parent
    {
        return false;
    }
    if let Some(prefix) = media_type_prefix
        && !node.entry.media_type.starts_with(prefix)
    {
        return false;
    }
    true
}

fn text_snippet(text: &str, needle: &str) -> Option<String> {
    let position = text.find(needle)?;
    let start = position.saturating_sub(40);
    let end = (position + needle.len() + 40).min(text.len());
    Some(text[start..end].to_string())
}

fn fuzzy_score(haystack: &str, needle: &str) -> Option<u32> {
    let mut score = 0u32;
    let mut position = 0usize;
    for ch in needle.chars() {
        let offset = haystack[position..].find(ch)?;
        score = score.saturating_add(1 + offset as u32);
        position += offset + ch.len_utf8();
    }
    Some(score)
}

fn parent_of(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_ranges_are_utf8_safe() {
        let text = "a".repeat(MAX_CHUNK_BYTES) + "é";
        let ranges = chunk_ranges(&text);
        assert!(ranges.len() >= 2);
        for (start, end) in ranges {
            assert!(text.is_char_boundary(start));
            assert!(text.is_char_boundary(end));
        }
    }

    #[test]
    fn fts_queries_are_escaped() {
        assert_eq!(
            fts_query("greece trip"),
            Some("\"greece\" OR \"trip\"".into())
        );
        assert_eq!(fts_query("   "), None);
        assert_eq!(fts_query("a\"b"), Some("\"a\" OR \"b\"".into()));
    }

    #[test]
    fn vectors_round_trip_and_rank_by_cosine() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = SqliteIndex::open(dir.path().join("index.sqlite3")).unwrap();
        index
            .db
            .execute(
                "INSERT INTO chunks(chunk_id,path,content_id,ordinal,text) VALUES
             ('a','a.txt','ca',0,'a'),
             ('b','b.txt','cb',0,'b')",
                [],
            )
            .unwrap();
        index.put_vector("a", "test-model", &[1.0, 0.0]).unwrap();
        index.put_vector("b", "test-model", &[0.0, 1.0]).unwrap();
        let hits = index.vector_search("test-model", &[0.9, 0.1], 2).unwrap();
        assert_eq!(hits[0].0, "a");
        assert!(hits[0].1 > hits[1].1);
    }
}
