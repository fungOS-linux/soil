//! Host implementation of `soil:vfs` and `soil:index` over `fungos-vfs`.
//!
//! The host owns canonical paths, the VFS namespace, hydration and the derived
//! index. A Spore sees only the capability contracts.

mod embeddings;
mod indexer;

use std::sync::{Arc, Mutex};

use anyhow::Context;
use fungos_vfs::{Hydration, NodeRole, Vfs, VfsError, normalize_path};
use wasmtime::component::{HasSelf, Linker};

wasmtime::component::bindgen!({ path: "wit", world: "host" });

pub use embeddings::EmbeddingProvider;
#[cfg(feature = "model2vec")]
pub use embeddings::Model2VecEmbedder;
#[cfg(feature = "remote")]
pub use embeddings::RemoteEmbeddingProvider;
use indexer::{IndexBackend, Node, NodeKind, SqliteIndex};
pub use soil::index::index;
pub use soil::vfs::vfs;

pub(crate) const MAX_PAGE: u32 = 512;
const MAX_RANGE: u32 = 1024 * 1024;

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
    index: Arc<Mutex<Box<dyn IndexBackend>>>,
    embeddings: Option<Arc<dyn EmbeddingProvider>>,
}

impl VfsHost {
    pub fn open(root: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        Self::open_with_role(root, NodeRole::Full)
    }

    /// Open with the default derived index at `<root>/.fungos/soil-index.sqlite3`.
    pub fn open_with_role(
        root: impl AsRef<std::path::Path>,
        role: NodeRole,
    ) -> anyhow::Result<Self> {
        let root = root.as_ref();
        let index_path = root.join(".fungos/soil-index.sqlite3");
        Self::open_with_role_and_index(root, role, index_path)
    }

    /// Open with an explicit derived-index database. The database is a cache and
    /// can be deleted at any time; it must not live inside `records`.
    pub fn open_with_index(
        root: impl AsRef<std::path::Path>,
        index_path: impl AsRef<std::path::Path>,
    ) -> anyhow::Result<Self> {
        Self::open_with_role_and_index(root, NodeRole::Full, index_path)
    }

    pub fn open_with_role_and_index(
        root: impl AsRef<std::path::Path>,
        role: NodeRole,
        index_path: impl AsRef<std::path::Path>,
    ) -> anyhow::Result<Self> {
        let mut vfs = Vfs::open_with_role(root, role).context("open fungos-vfs")?;
        let mut index = SqliteIndex::open(index_path).context("open soil index")?;
        index.refresh(&mut vfs).context("refresh soil index")?;
        Ok(Self {
            vfs: Arc::new(Mutex::new(vfs)),
            hydrator: None,
            index: Arc::new(Mutex::new(Box::new(index))),
            embeddings: None,
        })
    }

    pub fn with_hydrator(mut self, hydrator: Arc<dyn ContentHydrator>) -> Self {
        self.hydrator = Some(hydrator);
        self
    }

    /// Opt in to semantic indexing with an explicit host-selected provider.
    pub fn with_embeddings(
        mut self,
        embeddings: Arc<dyn EmbeddingProvider>,
    ) -> anyhow::Result<Self> {
        self.embeddings = Some(embeddings);
        self.refresh_embeddings()?;
        Ok(self)
    }

    pub fn refresh_index(&self) -> anyhow::Result<index::IndexState> {
        {
            let mut vfs = self.vfs.lock().expect("vfs poisoned");
            let mut index = self.index.lock().expect("index poisoned");
            index.refresh(&mut vfs)?;
        }
        self.refresh_embeddings()?;
        Ok(self.index_read().state())
    }

    fn refresh_embeddings(&self) -> anyhow::Result<()> {
        let Some(provider) = &self.embeddings else {
            return Ok(());
        };
        let model = provider.model_id();
        loop {
            let chunks = {
                let index = self.index_read();
                index.missing_chunks(model, 64)?
            };
            if chunks.is_empty() {
                break;
            }
            let texts: Vec<_> = chunks.iter().map(|(_, text)| text.clone()).collect();
            let vectors = provider.embed(&texts)?;
            if vectors.len() != chunks.len() {
                anyhow::bail!(
                    "embedding provider returned {} vectors for {} chunks",
                    vectors.len(),
                    chunks.len()
                );
            }
            let mut index = self.index.lock().expect("index poisoned");
            for ((chunk_id, _), vector) in chunks.iter().zip(vectors) {
                if vector.len() != provider.dimension() {
                    anyhow::bail!(
                        "embedding dimension {} does not match model {}",
                        vector.len(),
                        provider.dimension()
                    );
                }
                index.put_vector(chunk_id, model, &vector)?;
            }
        }
        Ok(())
    }

    fn semantic_search(&self, query: &index::Query) -> Result<index::Page, index::Error> {
        let Some(provider) = &self.embeddings else {
            return Err(index::Error::Unsupported);
        };
        let vector = provider
            .embed(std::slice::from_ref(&query.text))
            .map_err(index_error)?
            .into_iter()
            .next()
            .ok_or(index::Error::Io)?;
        let parent = query
            .parent
            .as_deref()
            .map(normalize_parent)
            .transpose()
            .map_err(|_| index::Error::Invalid)?;
        let index = self.index_read();
        let raw = index
            .semantic_search(provider.model_id(), &vector, MAX_PAGE as usize * 4)
            .map_err(index_error)?;
        let mut hits = Vec::new();
        for (path, score) in raw {
            let Some(node) = index.entry(&path) else {
                continue;
            };
            if let Some(parent) = &parent {
                let start = if parent.is_empty() {
                    0
                } else {
                    parent.len() + 1
                };
                let rest = path.get(start..).unwrap_or_default();
                let direct = path.starts_with(parent) && !rest.contains('/');
                if !direct {
                    continue;
                }
            }
            if let Some(prefix) = &query.media_type_prefix
                && !node.entry.media_type.starts_with(prefix)
            {
                continue;
            }
            hits.push(index::Hit {
                path,
                score: (score.max(0.0) * 1_000_000.0) as u32,
                snippet: Some(node.text.chars().take(160).collect()),
                matched_by: Some(index::MatchSource::Embedding),
            });
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
        let start = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let page = hits
            .into_iter()
            .skip(start)
            .take(query.limit as usize)
            .collect();
        Ok(index::Page {
            hits: page,
            next: None,
            generation: index.state().generation,
        })
    }

    fn index_read(&self) -> std::sync::MutexGuard<'_, Box<dyn IndexBackend>> {
        self.index.lock().expect("index poisoned")
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
                vfs.get_or_hydrate(path, &wrapped).map_err(vfs_error)?
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
        VfsError::InvalidContentId(_)
        | VfsError::Conflict { .. }
        | VfsError::AlreadyExists(_)
        | VfsError::Leased { .. } => vfs::Error::Io,
        VfsError::RoleMismatch { .. } => vfs::Error::Denied,
    }
}

fn index_error(error: anyhow::Error) -> index::Error {
    eprintln!("soil index: {error:#}");
    index::Error::Io
}

impl vfs::Host for VfsHost {
    fn ls(&mut self, parent: String, offset: u64, limit: u32) -> Result<vfs::Page, vfs::Error> {
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
            .nodes()
            .values()
            .filter(|node| !node.entry.path.is_empty() && parent_of(&node.entry.path) == parent)
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

    fn read(&mut self, path: String, offset: u64, length: u32) -> Result<vfs::Chunk, vfs::Error> {
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
        self.index_read().browse(&parent, order, offset, limit)
    }

    fn search(&mut self, query: index::Query) -> Result<index::Page, index::Error> {
        if query.mode == Some(index::Mode::Semantic) {
            return self.semantic_search(&query);
        }
        self.index_read().search(&query)
    }

    fn state(&mut self) -> Result<index::IndexState, index::Error> {
        Ok(self.index_read().state())
    }

    fn refresh(&mut self) -> Result<index::IndexState, index::Error> {
        self.refresh_index().map_err(index_error)
    }
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
        assert!(
            page.hits
                .iter()
                .all(|hit| hit.path.contains("greece") || hit.path == "notes.txt")
        );
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
    fn regex_search_indexes_code_extensions_and_fuzzy_text() {
        let dir = tempfile::tempdir().unwrap();
        let mut vfs = Vfs::open(dir.path()).unwrap();
        vfs.put(
            "src/main.rs",
            b"fn main() {\n    run();\n}\n",
            "application/octet-stream",
            None,
        )
        .unwrap();
        let mut host = VfsHost::open(dir.path()).unwrap();
        let page = host
            .search(index::Query {
                text: r"fn\s+main".into(),
                mode: Some(index::Mode::Regex),
                parent: None,
                media_type_prefix: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].path, "src/main.rs");
        assert!(
            page.hits[0]
                .snippet
                .as_deref()
                .is_some_and(|s| s.contains("fn main"))
        );

        let page = host
            .search(index::Query {
                text: "fn main".into(),
                mode: Some(index::Mode::Fuzzy),
                parent: None,
                media_type_prefix: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert!(page.hits.iter().any(|hit| hit.path == "src/main.rs"));
    }

    #[test]
    fn full_text_search_uses_fts_and_can_filter_by_parent() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut host = VfsHost::open(dir.path()).unwrap();
        let page = host
            .search(index::Query {
                text: "trip".into(),
                mode: Some(index::Mode::FullText),
                parent: None,
                media_type_prefix: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].path, "notes.txt");
        assert_eq!(page.hits[0].matched_by, Some(index::MatchSource::Text));

        let page = host
            .search(index::Query {
                text: "trip".into(),
                mode: Some(index::Mode::FullText),
                parent: Some("photos".into()),
                media_type_prefix: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert!(page.hits.is_empty());
    }

    #[test]
    fn persistent_index_survives_reopen_without_rebuilding() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let index_path = dir.path().join("derived.sqlite3");
        let state = {
            let mut host = VfsHost::open_with_index(dir.path(), &index_path).unwrap();
            host.state().unwrap()
        };
        assert!(index_path.exists());
        let mut host = VfsHost::open_with_index(dir.path(), &index_path).unwrap();
        let reopened = host.state().unwrap();
        assert_eq!(reopened.generation, state.generation);
        assert_eq!(reopened.entries, state.entries);

        let mut vfs = Vfs::open(dir.path()).unwrap();
        vfs.put("notes/new.txt", b"a new semantic note", "text/plain", None)
            .unwrap();
        let state = host.refresh_index().unwrap();
        assert_eq!(state.generation, 2);
        let page = host
            .search(index::Query {
                text: "semantic".into(),
                mode: Some(index::Mode::FullText),
                parent: None,
                media_type_prefix: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits[0].path, "notes/new.txt");
    }

    struct FakeEmbeddings;

    impl EmbeddingProvider for FakeEmbeddings {
        fn model_id(&self) -> &str {
            "fake:test"
        }
        fn dimension(&self) -> usize {
            2
        }
        fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| {
                    if text.to_lowercase().contains("trip") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect())
        }
    }

    #[test]
    fn semantic_search_uses_the_host_embedding_provider() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut host = VfsHost::open(dir.path())
            .unwrap()
            .with_embeddings(Arc::new(FakeEmbeddings))
            .unwrap();
        let page = host
            .search(index::Query {
                text: "trip".into(),
                mode: Some(index::Mode::Semantic),
                parent: None,
                media_type_prefix: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].path, "notes.txt");
        assert_eq!(page.hits[0].matched_by, Some(index::MatchSource::Embedding));
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
