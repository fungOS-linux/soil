//! Host implementation of `soil:vfs` and `soil:index` over `fungos-vfs`.
//!
//! The host owns canonical paths, the VFS namespace, hydration and the derived
//! index. A Spore sees only the capability contracts.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use fungos_vfs::{FileEntry, Hydration, NodeRole, Vfs, VfsError, normalize_path};
use wasmtime::component::{HasSelf, Linker};

wasmtime::component::bindgen!({ path: "wit", world: "host" });

use soil::index::index as index;
use soil::vfs::vfs as vfs;

const MAX_PAGE: u32 = 512;
const MAX_RANGE: u32 = 1024 * 1024;
const MAX_INDEX_TEXT: usize = 64 * 1024;

/// Optional content transport used when a VFS path is only a placeholder.
pub trait ContentHydrator: Send + Sync {
    fn fetch(&self, path: &str, content_id: &str, size: u64) -> Result<Vec<u8>, VfsError>;
}

struct DynHydrator(Arc<dyn ContentHydrator>);

impl fungos_vfs::Hydrator for DynHydrator {
    fn fetch(
        &self,
        path: &str,
        content_id: &fungos_vfs::ContentId,
        size: u64,
    ) -> Result<Vec<u8>, VfsError> {
        self.0.fetch(path, &content_id.to_string(), size)
    }
}

#[derive(Clone)]
pub struct VfsHost {
    vfs: Arc<Mutex<Vfs>>,
    hydrator: Option<Arc<dyn ContentHydrator>>,
    index: Arc<RwLock<Index>>,
}

impl VfsHost {
    pub fn open(root: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        Self::open_with_role(root, NodeRole::Full)
    }

    pub fn open_with_role(
        root: impl AsRef<std::path::Path>,
        role: NodeRole,
    ) -> anyhow::Result<Self> {
        let vfs = Vfs::open_with_role(root, role).context("open fungos-vfs")?;
        let host = Self {
            vfs: Arc::new(Mutex::new(vfs)),
            hydrator: None,
            index: Arc::new(RwLock::new(Index::empty())),
        };
        host.refresh_index()?;
        Ok(host)
    }

    pub fn with_hydrator(mut self, hydrator: Arc<dyn ContentHydrator>) -> Self {
        self.hydrator = Some(hydrator);
        self
    }

    pub fn refresh_index(&self) -> anyhow::Result<index::IndexState> {
        let snapshot = {
            let mut vfs = self.vfs.lock().expect("vfs poisoned");
            Index::rebuild(&mut vfs)?
        };
        let mut snapshot = snapshot;
        let previous = self.index.read().expect("index poisoned").generation;
        snapshot.generation = previous.saturating_add(1);
        let state = snapshot.state();
        *self.index.write().expect("index poisoned") = snapshot;
        Ok(state)
    }

    fn index_read(&self) -> std::sync::RwLockReadGuard<'_, Index> {
        self.index.read().expect("index poisoned")
    }

    fn read_path(&self, path: &str, offset: u64, length: u32) -> Result<Vec<u8>, vfs::Error> {
        if length == 0 || length > MAX_RANGE {
            return Err(vfs::Error::OverBudget);
        }
        let mut vfs = self.vfs.lock().expect("vfs poisoned");
        let mut bytes = match vfs.get(path) {
            Ok(bytes) => bytes,
            Err(VfsError::NotHydrated(_)) => {
                let Some(hydrator) = &self.hydrator else {
                    return Err(vfs::Error::NotHydrated);
                };
                let wrapped = DynHydrator(hydrator.clone());
                vfs.get_or_hydrate(path, &wrapped)
                    .map_err(vfs_error)?
            }
            Err(error) => return Err(vfs_error(error)),
        };
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start > bytes.len() {
            return Err(vfs::Error::Invalid);
        }
        let end = start.saturating_add(length as usize).min(bytes.len());
        Ok(bytes.drain(start..end).collect())
    }
}

fn vfs_error(error: VfsError) -> vfs::Error {
    match error {
        VfsError::NotFound(_) => vfs::Error::NotFound,
        VfsError::NotHydrated(_) => vfs::Error::NotHydrated,
        VfsError::InvalidPath(_) => vfs::Error::Invalid,
        VfsError::ReadOnly => vfs::Error::Denied,
        VfsError::Io(_) | VfsError::Sql(_) | VfsError::Transport(_) => vfs::Error::Io,
        VfsError::InvalidContentId(_) | VfsError::Conflict { .. } | VfsError::Leased { .. } => {
            vfs::Error::Io
        }
        VfsError::RoleMismatch { .. } => vfs::Error::Denied,
    }
}

fn index_error(error: anyhow::Error) -> index::Error {
    eprintln!("soil index: {error:#}");
    index::Error::Io
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

struct Index {
    nodes: BTreeMap<String, Node>,
    generation: u64,
    rebuilt_at_ms: u64,
    ready: bool,
}

#[derive(Clone)]
struct Node {
    entry: FileEntry,
    kind: NodeKind,
    child_count: u64,
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    File,
    Directory,
}

impl Index {
    fn empty() -> Self {
        Self {
            nodes: BTreeMap::new(),
            generation: 0,
            rebuilt_at_ms: 0,
            ready: false,
        }
    }

    fn state(&self) -> index::IndexState {
        index::IndexState {
            ready: self.ready,
            generation: self.generation,
            entries: self.nodes.len() as u64,
            rebuilt_at_ms: self.rebuilt_at_ms,
        }
    }

    fn rebuild(vfs: &mut Vfs) -> anyhow::Result<Self> {
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

        let files = vfs.list("")?;
        for entry in files {
            let path = entry.path.clone();
            add_ancestors(&mut nodes, &path);
            let text = if entry.media_type.starts_with("text/")
                && entry.hydration == Hydration::Present
                && entry.size <= MAX_INDEX_TEXT as u64
            {
                vfs.get(&path)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_lowercase())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            nodes.insert(
                path.clone(),
                Node {
                    entry,
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

        Ok(Self {
            nodes,
            generation: 1,
            rebuilt_at_ms: now_ms(),
            ready: true,
        })
    }

    fn entry(&self, path: &str) -> Option<&Node> {
        self.nodes.get(path)
    }
}

fn add_ancestors(nodes: &mut BTreeMap<String, Node>, path: &str) {
    let mut prefix = String::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        nodes.entry(prefix.clone()).or_insert_with(|| {
            Node {
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
            }
        });
    }
}

impl vfs::Host for VfsHost {
    fn ls(
        &mut self,
        parent: String,
        offset: u64,
        limit: u32,
    ) -> Result<vfs::Page, vfs::Error> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(vfs::Error::OverBudget);
        }
        let parent = normalize_parent(&parent)?;
        let index = self.index_read();
        let Some(node) = index.entry(&parent) else {
            return Err(vfs::Error::NotFound);
        };
        if node.kind != NodeKind::Directory {
            return Err(vfs::Error::NotDirectory);
        }
        let mut children: Vec<_> = index
            .nodes
            .values()
            .filter(|node| {
                !node.entry.path.is_empty() && parent_of(&node.entry.path) == parent
            })
            .collect();
        children.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let entries: Vec<_> = children
            .into_iter()
            .skip(start)
            .take(limit as usize)
            .map(to_entry)
            .collect();
        let next = if offset.saturating_add(entries.len() as u64) < node.child_count {
            Some(offset + entries.len() as u64)
        } else {
            None
        };
        Ok(vfs::Page { entries, next })
    }

    fn stat(&mut self, path: String) -> Result<vfs::Entry, vfs::Error> {
        let path = normalize_parent(&path)?;
        let index = self.index_read();
        let node = index.entry(&path).ok_or(vfs::Error::NotFound)?;
        Ok(to_entry(node))
    }

    fn read(
        &mut self,
        path: String,
        offset: u64,
        length: u32,
    ) -> Result<vfs::Chunk, vfs::Error> {
        let path = normalize_parent(&path)?;
        let kind = self
            .index_read()
            .entry(&path)
            .map(|node| node.kind)
            .ok_or(vfs::Error::NotFound)?;
        if kind != NodeKind::File {
            return Err(vfs::Error::IsDirectory);
        }
        let content_id = self
            .index_read()
            .entry(&path)
            .map(|node| node.entry.content_id.to_string())
            .unwrap_or_default();
        let data = self.read_path(&path, offset, length)?;
        let size = self
            .index_read()
            .entry(&path)
            .map(|node| node.entry.size)
            .unwrap_or(0);
        Ok(vfs::Chunk {
            offset,
            eof: offset.saturating_add(data.len() as u64) >= size,
            data,
            content_id,
        })
    }
}

fn normalize_parent(value: &str) -> Result<String, vfs::Error> {
    if value.is_empty() {
        return Ok(String::new());
    }
    normalize_path(value).map_err(|_| vfs::Error::Invalid)
}

fn parent_of(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default()
}

fn to_entry(node: &Node) -> vfs::Entry {
    vfs::Entry {
        path: node.entry.path.clone(),
        name: node
            .entry
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&node.entry.path)
            .to_string(),
        kind: if node.kind == NodeKind::Directory {
            vfs::Kind::Directory
        } else {
            vfs::Kind::File
        },
        size: node.entry.size,
        content_id: node.entry.content_id.to_string(),
        media_type: node.entry.media_type.clone(),
        version: node.entry.version,
        hydration: if node.entry.hydration == Hydration::Placeholder {
            vfs::Hydration::Placeholder
        } else {
            vfs::Hydration::Present
        },
        pinned: node.entry.pinned,
        updated_at_ms: node.entry.updated_at_ms.max(0) as u64,
        child_count: node.child_count,
    }
}

impl index::Host for VfsHost {
    fn browse(
        &mut self,
        parent: String,
        order: index::Order,
        offset: u64,
        limit: u32,
    ) -> Result<index::Page, index::Error> {
        if limit == 0 || limit > MAX_PAGE {
            return Err(index::Error::OverBudget);
        }
        let parent = normalize_parent(&parent).map_err(|_| index::Error::Invalid)?;
        let index = self.index_read();
        if !index.ready {
            return Err(index::Error::NotReady);
        }
        let Some(parent_node) = index.entry(&parent) else {
            return Err(index::Error::Invalid);
        };
        if parent_node.kind != NodeKind::Directory {
            return Err(index::Error::Invalid);
        }
        let mut nodes: Vec<_> = index
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
            index::Order::Size => nodes.sort_by_key(|node| (node.entry.size, node.entry.path.clone())),
            index::Order::SizeDesc => {
                nodes.sort_by_key(|node| (std::cmp::Reverse(node.entry.size), node.entry.path.clone()))
            }
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
            generation: index.generation,
        })
    }

    fn search(&mut self, query: index::Query) -> Result<index::Page, index::Error> {
        if query.limit == 0 || query.limit > MAX_PAGE {
            return Err(index::Error::OverBudget);
        }
        if query.text.trim().is_empty() {
            return Err(index::Error::Invalid);
        }
        let parent = query
            .parent
            .as_deref()
            .map(normalize_parent)
            .transpose()
            .map_err(|_| index::Error::Invalid)?;
        let index = self.index_read();
        if !index.ready {
            return Err(index::Error::NotReady);
        }
        let mode = query.mode.unwrap_or(index::Mode::Substring);
        if mode == index::Mode::Semantic {
            return Err(index::Error::Unsupported);
        }
        let needle = query.text.to_lowercase();
        let mut hits = Vec::new();
        for node in index.nodes.values() {
            if node.kind == NodeKind::Directory {
                continue;
            }
            if let Some(parent) = &parent
                && parent_of(&node.entry.path) != *parent
            {
                continue;
            }
            if let Some(prefix) = &query.media_type_prefix
                && !node.entry.media_type.starts_with(prefix)
            {
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
                index::Mode::Fuzzy => fuzzy_score(&path, &needle)
                    .map(|score| (score, index::MatchSource::Path)),
                index::Mode::FullText => path
                    .contains(&needle)
                    .then_some((1, index::MatchSource::Path))
                    .or_else(|| text.contains(&needle).then_some((2, index::MatchSource::Text))),
                index::Mode::Semantic => unreachable!(),
            }) else {
                continue;
            };
            let snippet = text.find(&needle).map(|position| {
                let start = position.saturating_sub(40);
                let end = (position + needle.len() + 40).min(text.len());
                text[start..end].to_string()
            });
            hits.push(index::Hit {
                path: node.entry.path.clone(),
                score,
                snippet,
                matched_by: Some(matched_by),
            });
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
        let start = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let page: Vec<_> = hits.into_iter().skip(start).take(query.limit as usize).collect();
        Ok(index::Page {
            hits: page,
            next: None,
            generation: index.generation,
        })
    }

    fn state(&mut self) -> Result<index::IndexState, index::Error> {
        Ok(self.index_read().state())
    }

    fn refresh(&mut self) -> Result<index::IndexState, index::Error> {
        self.refresh_index().map_err(index_error)
    }
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

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: vfs::Host + index::Host + 'static,
{
    vfs::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    index::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soil::index::index::Host as _;
    use crate::soil::vfs::vfs::Host as _;
    use std::path::Path;

    fn seed(root: &Path) {
        let mut vfs = Vfs::open(root).unwrap();
        vfs.put("notes.txt", b"hello greece trip", "text/plain", None)
            .unwrap();
        vfs.put("photos/greece-01.jpg", b"jpeg-one", "image/jpeg", None)
            .unwrap();
        vfs.put("photos/greece-02.jpg", b"jpeg-two", "image/jpeg", None)
            .unwrap();
    }

    #[test]
    fn lists_and_searches_the_derived_index() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut host = VfsHost::open(dir.path()).unwrap();

        let root = host.stat(String::new()).unwrap();
        assert_eq!(root.kind, vfs::Kind::Directory);
        assert_eq!(root.child_count, 2);

        let page = host.ls(String::new(), 0, 10).unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].path, "notes.txt");
        assert_eq!(page.entries[1].path, "photos");

        let page = host
            .search(index::Query {
                text: "greece".into(),
                mode: None,
                parent: None,
                media_type_prefix: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 3);
        assert!(page.hits.iter().all(|hit| hit.path.contains("greece") || hit.path == "notes.txt"));
    }

    #[test]
    fn reads_ranges_and_reports_eof() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut host = VfsHost::open(dir.path()).unwrap();
        let chunk = host.read("notes.txt".into(), 6, 6).unwrap();
        assert_eq!(chunk.data, b"greece");
        assert!(!chunk.eof);
        let tail = host.read("notes.txt".into(), 12, 64).unwrap();
        assert_eq!(tail.data, b" trip");
        assert!(tail.eof);
        assert_eq!(
            tail.content_id,
            fungos_vfs::ContentId::from_bytes(b"hello greece trip").to_string()
        );
    }

    #[test]
    fn browse_orders_by_size_and_paginates() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut host = VfsHost::open(dir.path()).unwrap();
        let page = host
            .browse("photos".into(), index::Order::SizeDesc, 0, 1)
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].path, "photos/greece-01.jpg");
        assert_eq!(page.next, Some(1));
    }

    #[test]
    fn semantic_search_is_an_explicit_unsupported_error() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut host = VfsHost::open(dir.path()).unwrap();
        let error = host
            .search(index::Query {
                text: "trip".into(),
                mode: Some(index::Mode::Semantic),
                parent: None,
                media_type_prefix: None,
                offset: 0,
                limit: 10,
            })
            .unwrap_err();
        assert_eq!(error, index::Error::Unsupported);
    }
}
