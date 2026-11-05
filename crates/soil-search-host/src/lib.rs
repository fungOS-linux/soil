//! Host-composed global search over granted local sources.
//!
//! VFS hits come from the derived `soil:index` backend; record hits come from
//! the materialized `fungos-records` view. This crate merges them into one
//! bounded page. It does not grant access: the host supplies the roots and
//! approved books.

mod git;
mod tgrep;

use std::sync::{Arc, Mutex};

use anyhow::Context;
use fungos_records::{Record, Store};
use git::GitIndex;
use soil_vfs_host::{EmbeddingProvider, VfsHost, index};
use tgrep::TgrepSource;
use wasmtime::component::{HasSelf, Linker};

wasmtime::component::bindgen!({ path: "wit", world: "search-host" });

use soil::search::search as search_api;

pub use soil::search::search as api;

const MAX_PAGE: u32 = 512;

pub struct SearchService {
    vfs: Option<VfsHost>,
    records: Option<Mutex<Store>>,
    books: Vec<String>,
    git: Option<GitIndex>,
    tgrep: Option<TgrepSource>,
    generation: u64,
}

impl SearchService {
    pub fn new(vfs: Option<VfsHost>, records: Option<Store>, books: Vec<String>) -> Self {
        Self {
            vfs,
            records: records.map(Mutex::new),
            books,
            git: None,
            tgrep: None,
            generation: 1,
        }
    }

    /// Enable semantic file search with the host-selected embedding provider.
    pub fn with_embeddings(
        mut self,
        embeddings: Arc<dyn EmbeddingProvider>,
    ) -> anyhow::Result<Self> {
        if let Some(vfs) = self.vfs.take() {
            self.vfs = Some(vfs.with_embeddings(embeddings)?);
        }
        self.generation = self.generation.saturating_add(1);
        Ok(self)
    }

    pub fn open(
        vfs_root: impl AsRef<std::path::Path>,
        records_root: impl AsRef<std::path::Path>,
        books: Vec<String>,
    ) -> anyhow::Result<Self> {
        let vfs = VfsHost::open(vfs_root).context("open soil:vfs host")?;
        let records_root = records_root.as_ref();
        let device = records_root.join("device_id");
        let records = Store::open(records_root, &device).context("open records store")?;
        Ok(Self::new(Some(vfs), Some(records), books))
    }

    /// Add explicit local Git roots. The host grants these; a Spore cannot add
    /// or discover a root.
    pub fn with_git_roots(mut self, roots: Vec<std::path::PathBuf>) -> Self {
        let mut git = GitIndex::new(roots);
        git.refresh();
        self.git = Some(git);
        self.generation = self.generation.saturating_add(1);
        self
    }

    /// Use Microsoft tgrep over an explicit root, which may be a VFS FUSE
    /// mount. Regex mode prefers tgrep when configured.
    pub fn with_tgrep(
        mut self,
        root: impl Into<std::path::PathBuf>,
        binary: impl Into<std::path::PathBuf>,
    ) -> Self {
        self.tgrep = Some(TgrepSource::new(root, binary));
        self.generation = self.generation.saturating_add(1);
        self
    }

    fn sources(&self) -> u32 {
        u32::from(self.vfs.is_some())
            + u32::from(self.records.is_some())
            + u32::from(self.git.is_some())
            + u32::from(self.tgrep.is_some())
    }

    fn snapshot(&self) -> search_api::SearchState {
        search_api::SearchState {
            ready: self.sources() > 0,
            generation: self.generation,
            sources: self.sources(),
        }
    }

    fn search(&mut self, query: search_api::Query) -> Result<search_api::Page, search_api::Error> {
        if query.limit == 0 || query.limit > MAX_PAGE {
            return Err(search_api::Error::OverBudget);
        }
        if query.text.trim().is_empty() {
            return Err(search_api::Error::Invalid);
        }
        let kinds = if query.kinds.is_empty() {
            vec![
                search_api::Kind::File,
                search_api::Kind::RecordEntry,
                search_api::Kind::Repository,
            ]
        } else {
            query.kinds.clone()
        };
        let mut hits = Vec::new();
        if kinds.contains(&search_api::Kind::File) {
            hits.extend(self.file_hits(&query)?);
        }
        if kinds.contains(&search_api::Kind::RecordEntry) {
            hits.extend(self.record_hits(&query)?);
        }
        if kinds.contains(&search_api::Kind::Repository) {
            hits.extend(self.git_hits(&query));
        }
        if kinds.contains(&search_api::Kind::Action) {
            hits.extend(self.action_hits(&query));
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
        let start = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let total = hits.len();
        let page: Vec<_> = hits
            .into_iter()
            .skip(start)
            .take(query.limit as usize)
            .collect();
        let consumed = query.offset.saturating_add(page.len() as u64);
        Ok(search_api::Page {
            hits: page,
            next: (consumed < total as u64).then_some(consumed),
            generation: self.generation,
        })
    }

    fn file_hits(
        &mut self,
        query: &search_api::Query,
    ) -> Result<Vec<search_api::Hit>, search_api::Error> {
        if query.mode == Some(search_api::Mode::Regex)
            && let Some(tgrep) = &self.tgrep
        {
            let hits = tgrep
                .search(&query.text, MAX_PAGE as usize)
                .map_err(|_| search_api::Error::Io)?;
            return Ok(hits
                .into_iter()
                .map(|hit| search_api::Hit {
                    kind: search_api::Kind::File,
                    id: hit.path.clone(),
                    title: hit.path.rsplit('/').next().unwrap_or(&hit.path).to_string(),
                    subtitle: Some(format!("{}:{}", hit.line, hit.column)),
                    snippet: (!hit.text.is_empty()).then_some(hit.text),
                    score: 1_000_000,
                    source: "tgrep".into(),
                    updated_at_ms: None,
                })
                .collect());
        }
        let Some(vfs) = self.vfs.as_mut() else {
            return Ok(Vec::new());
        };
        let index_query = index::Query {
            text: query.text.clone(),
            mode: Some(match query.mode.unwrap_or(search_api::Mode::Substring) {
                search_api::Mode::Substring => index::Mode::Substring,
                search_api::Mode::Fuzzy => index::Mode::Fuzzy,
                search_api::Mode::FullText => index::Mode::FullText,
                search_api::Mode::Semantic => index::Mode::Semantic,
                search_api::Mode::Regex => index::Mode::Regex,
            }),
            parent: None,
            media_type_prefix: None,
            offset: 0,
            limit: MAX_PAGE,
        };
        use soil_vfs_host::index::Host as _;
        let page = vfs.search(index_query).map_err(index_error)?;
        Ok(page
            .hits
            .into_iter()
            .map(|hit| search_api::Hit {
                kind: search_api::Kind::File,
                title: hit.path.rsplit('/').next().unwrap_or(&hit.path).to_string(),
                id: hit.path.clone(),
                subtitle: None,
                snippet: hit.snippet,
                score: hit.score,
                source: "vfs".into(),
                updated_at_ms: None,
            })
            .collect())
    }

    fn git_hits(&self, query: &search_api::Query) -> Vec<search_api::Hit> {
        let Some(git) = &self.git else {
            return Vec::new();
        };
        git.search(&query.text, MAX_PAGE as usize)
            .into_iter()
            .map(|(repo, score)| search_api::Hit {
                kind: search_api::Kind::Repository,
                id: repo.id.clone(),
                title: repo.name,
                subtitle: Some(match (repo.branch, repo.remote) {
                    (Some(branch), Some(remote)) => format!("{branch} · {remote}"),
                    (Some(branch), None) => branch,
                    (None, Some(remote)) => remote,
                    (None, None) => repo.id.clone(),
                }),
                snippet: None,
                score,
                source: "git".into(),
                updated_at_ms: None,
            })
            .collect()
    }

    fn action_hits(&self, query: &search_api::Query) -> Vec<search_api::Hit> {
        let Some(git) = &self.git else {
            return Vec::new();
        };
        git.actions(&query.text, MAX_PAGE as usize)
            .into_iter()
            .map(|(action, score)| search_api::Hit {
                kind: search_api::Kind::Action,
                id: action.id,
                title: action.title,
                subtitle: Some(action.subtitle),
                snippet: None,
                score,
                source: "git".into(),
                updated_at_ms: None,
            })
            .collect()
    }

    fn record_hits(
        &mut self,
        query: &search_api::Query,
    ) -> Result<Vec<search_api::Hit>, search_api::Error> {
        let Some(store) = self.records.as_ref() else {
            return Ok(Vec::new());
        };
        let books: Vec<&str> = self.books.iter().map(String::as_str).collect();
        let records = store
            .lock()
            .expect("records store poisoned")
            .search_books(&books, &query.text, MAX_PAGE as usize)
            .map_err(|_| search_api::Error::Io)?;
        let needle = query.text.to_lowercase();
        Ok(records
            .into_iter()
            .enumerate()
            .map(|(index, record)| record_hit(record, &needle, index))
            .collect())
    }
}

fn record_hit(record: Record, needle: &str, index: usize) -> search_api::Hit {
    let fields = record.fields;
    let title = ["display_name", "name", "title", "subject", "headline"]
        .iter()
        .find_map(|key| fields.get(key).and_then(serde_json::Value::as_str))
        .filter(|value| !value.is_empty())
        .unwrap_or(&record.entity_id)
        .to_string();
    let body = fields.to_string();
    let snippet = body.to_lowercase().find(needle).map(|position| {
        let start = position.saturating_sub(40);
        let end = (position + needle.len() + 80).min(body.len());
        body[start..end].to_string()
    });
    search_api::Hit {
        kind: search_api::Kind::RecordEntry,
        id: format!("{}/{}", record.book_id, record.entity_id),
        title,
        subtitle: Some(format!("{} · {}", record.book_id, record.schema_id)),
        snippet,
        score: 900_000u32.saturating_sub(index as u32),
        source: format!("records:{}", record.book_id),
        updated_at_ms: Some(record.updated_hlc),
    }
}

fn index_error(error: index::Error) -> search_api::Error {
    match error {
        index::Error::Denied => search_api::Error::Denied,
        index::Error::NotReady => search_api::Error::NotReady,
        index::Error::OverBudget => search_api::Error::OverBudget,
        index::Error::Invalid => search_api::Error::Invalid,
        index::Error::Stale => search_api::Error::Stale,
        index::Error::Io => search_api::Error::Io,
        index::Error::Unsupported => search_api::Error::Unsupported,
    }
}

impl search_api::Host for SearchService {
    fn search(&mut self, query: search_api::Query) -> Result<search_api::Page, search_api::Error> {
        SearchService::search(self, query)
    }

    fn state(&mut self) -> Result<search_api::SearchState, search_api::Error> {
        Ok(SearchService::snapshot(self))
    }

    fn refresh(&mut self) -> Result<search_api::SearchState, search_api::Error> {
        if let Some(vfs) = self.vfs.as_mut() {
            vfs.refresh_index().map_err(|_| search_api::Error::Io)?;
        }
        if let Some(git) = self.git.as_mut() {
            git.refresh();
        }
        self.generation = self.generation.saturating_add(1);
        Ok(self.snapshot())
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: search_api::Host + 'static,
{
    search_api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soil::search::search::Host as _;
    use fungos_vfs::Vfs;

    fn seed_vfs(root: &std::path::Path) {
        let mut vfs = Vfs::open(root).unwrap();
        vfs.put("notes/greece.txt", b"a greece trip", "text/plain", None)
            .unwrap();
        vfs.put("photos/paris.jpg", b"jpeg", "image/jpeg", None)
            .unwrap();
    }

    fn seed_records(root: &std::path::Path) -> Store {
        let mut store = Store::open(root, &root.join("device_id")).unwrap();
        store
            .put_record(
                "personal/avery/contacts",
                "ada",
                "fungos.contacts/contact",
                1,
                serde_json::json!({
                    "display_name": "Ada Lovelace",
                    "emails": [{"label": "work", "value": "ada@example.invalid"}]
                }),
            )
            .unwrap();
        store
    }

    struct FakeEmbeddings;

    impl EmbeddingProvider for FakeEmbeddings {
        fn model_id(&self) -> &str {
            "fake:global"
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
    fn semantic_file_search_uses_the_host_provider() {
        let dir = tempfile::tempdir().unwrap();
        seed_vfs(dir.path());
        let vfs = VfsHost::open(dir.path()).unwrap();
        let mut host = SearchService::new(Some(vfs), None, vec![])
            .with_embeddings(Arc::new(FakeEmbeddings))
            .unwrap();
        let page = host
            .search(search_api::Query {
                text: "trip".into(),
                kinds: vec![search_api::Kind::File],
                mode: Some(search_api::Mode::Semantic),
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].id, "notes/greece.txt");
    }

    #[cfg(unix)]
    #[test]
    fn tgrep_backs_regex_file_search() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("tgrep");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
printf '%s\n' '{"type":"match","data":{"path":{"text":"/repo/src/main.rs"},"line_number":7,"submatches":[{"start":4}],"lines":{"text":"fn main() {\n"}}}'
"#
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut host = SearchService::new(None, None, vec![]).with_tgrep("/repo", &binary);
        let page = host
            .search(search_api::Query {
                text: "fn main".into(),
                kinds: vec![search_api::Kind::File],
                mode: Some(search_api::Mode::Regex),
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].source, "tgrep");
        assert_eq!(page.hits[0].id, "src/main.rs");
    }

    #[test]
    fn searches_git_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("fpl/dock");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            repo.join(".git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/acme/dock.git\n",
        )
        .unwrap();
        let mut host =
            SearchService::new(None, None, vec![]).with_git_roots(vec![dir.path().to_path_buf()]);
        let page = host
            .search(search_api::Query {
                text: "dock".into(),
                kinds: vec![search_api::Kind::Repository],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].kind, search_api::Kind::Repository);
        assert_eq!(page.hits[0].id, "fpl/dock");
        assert_eq!(page.hits[0].title, "dock");
    }

    #[test]
    fn merges_files_and_records() {
        let dir = tempfile::tempdir().unwrap();
        seed_vfs(dir.path());
        let store = seed_records(&dir.path().join("records"));
        let vfs = VfsHost::open(dir.path()).unwrap();
        let mut host = SearchService::new(
            Some(vfs),
            Some(store),
            vec!["personal/avery/contacts".into()],
        );
        let page = host
            .search(search_api::Query {
                text: "greece".into(),
                kinds: vec![],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].kind, search_api::Kind::File);

        let page = host
            .search(search_api::Query {
                text: "ada".into(),
                kinds: vec![search_api::Kind::RecordEntry],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].kind, search_api::Kind::RecordEntry);
        assert_eq!(page.hits[0].title, "Ada Lovelace");
    }

    #[test]
    fn scope_filters_and_refresh_generation() {
        let dir = tempfile::tempdir().unwrap();
        seed_vfs(dir.path());
        let store = seed_records(&dir.path().join("records"));
        let vfs = VfsHost::open(dir.path()).unwrap();
        let mut host = SearchService::new(
            Some(vfs),
            Some(store),
            vec!["personal/avery/contacts".into()],
        );
        let before = host.state().unwrap().generation;
        let page = host
            .search(search_api::Query {
                text: "paris".into(),
                kinds: vec![search_api::Kind::RecordEntry],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert!(page.hits.is_empty());
        let after = host.refresh().unwrap().generation;
        assert_eq!(after, before + 1);
    }
}
