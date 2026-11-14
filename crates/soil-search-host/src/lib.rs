//! Host-composed global search over granted local sources.
//!
//! VFS hits come from the derived `soil:index` backend; record hits come from
//! the materialized `fungos-records` view. This crate merges them into one
//! bounded page. It does not grant access: the host supplies the roots and
//! approved books.

pub mod applications;
mod discovery;
mod git;
pub mod graph;
mod launcher;
mod mru;
mod store;
mod tgrep;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use discovery::{
    ApplicationEntry, ApplicationIndex, PeerIndex, ServiceIndex, SporeEntry, SporeIndex,
};
use fungos_records::{Record, Store};
use git::GitIndex;
use graph::GraphIndex;
use mru::{MruCache, hit_from_usage, now_ms, usage_from_hit};
use soil_marbles::MarblesIndex;
use ::soil::vfs_host::{EmbeddingProvider, VfsHost, index};
use tgrep::TgrepSource;
use wasmtime::component::{HasSelf, Linker};

wasmtime::component::bindgen!({ path: "wit/search.wit", world: "search-host" });

pub use launcher::{LauncherConfig, Profile};
pub use mru::{Usage, recency};
pub use crate::soil::search::mentions as mention_api;
use crate::soil::search::search as search_api;

pub use crate::soil::search::search as api;

/// The wire name of a kind. One table, because the HTTP surface, the usage cache
/// and anything else that names a kind would drift apart in a second one.
pub fn kind_name(kind: api::Kind) -> &'static str {
    match kind {
        api::Kind::File => "file",
        api::Kind::RecordEntry => "record-entry",
        api::Kind::Repository => "repository",
        api::Kind::Spore => "spore",
        api::Kind::Peer => "peer",
        api::Kind::Service => "service",
        api::Kind::Application => "application",
        api::Kind::Action => "action",
        api::Kind::Task => "task",
        api::Kind::Principal => "principal",
    }
}

pub fn kind_from_name(name: &str) -> Option<api::Kind> {
    Some(match name.trim() {
        "file" => api::Kind::File,
        "record-entry" => api::Kind::RecordEntry,
        "repository" => api::Kind::Repository,
        "spore" => api::Kind::Spore,
        "peer" => api::Kind::Peer,
        "service" => api::Kind::Service,
        "application" => api::Kind::Application,
        "action" => api::Kind::Action,
        "task" => api::Kind::Task,
        "principal" => api::Kind::Principal,
        _ => return None,
    })
}

const MAX_PAGE: u32 = 512;
const MAX_MENTIONS: u32 = 64;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrincipalCandidate {
    pub actor: String,
    pub display_name: Option<String>,
    pub teams: Vec<String>,
    pub roles: Vec<String>,
    pub visibility: String,
}

pub struct SearchService {
    vfs: Option<VfsHost>,
    records: Option<Mutex<Store>>,
    books: Vec<String>,
    graph: GraphIndex,
    git: Option<GitIndex>,
    tgrep: Option<TgrepSource>,
    spores: Option<SporeIndex>,
    peers: Option<PeerIndex>,
    services: Option<ServiceIndex>,
    applications: Option<ApplicationIndex>,
    marbles: Option<MarblesIndex>,
    principals: Vec<PrincipalCandidate>,
    mru: MruCache,
    launcher: LauncherConfig,
    launcher_path: Option<std::path::PathBuf>,
    generation: u64,
}

impl SearchService {
    pub fn new(vfs: Option<VfsHost>, records: Option<Store>, books: Vec<String>) -> Self {
        let records = records.map(Mutex::new);
        let graph = records
            .as_ref()
            .map(|store| GraphIndex::build(&store.lock().expect("records store poisoned"), &books))
            .unwrap_or_default();
        Self {
            vfs,
            records,
            books,
            graph,
            git: None,
            tgrep: None,
            spores: None,
            peers: None,
            services: None,
            applications: None,
            marbles: None,
            principals: Vec::new(),
            mru: MruCache::in_memory(),
            launcher: LauncherConfig::default(),
            launcher_path: None,
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

    /// Add explicit local Spore package roots. The host grants these; a Spore
    /// cannot discover or add a root.
    pub fn with_spore_roots(mut self, roots: Vec<std::path::PathBuf>) -> Self {
        self.spores = Some(SporeIndex::new(roots));
        self.generation = self.generation.saturating_add(1);
        self
    }

    /// Use an inventory the host discovered from this machine's bundles. The
    /// dynamically scanned counterpart of `with_applications_snapshot`.
    pub fn with_installed_applications(
        mut self,
        apps: &[crate::applications::InstalledApp],
    ) -> Self {
        self.applications = Some(ApplicationIndex::from_installed(apps));
        self.generation = self.generation.saturating_add(1);
        self
    }

    /// Load an explicit `mycelium peers --json` snapshot. The host chooses the
    /// file; search never talks to the daemon or opens a network connection.
    pub fn with_mycelium_peers(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.peers = Some(PeerIndex::load(path.into()));
        self.generation = self.generation.saturating_add(1);
        self
    }

    /// Load an explicit Mycelium/Unibus service catalog snapshot, e.g.
    /// `mycelium services --json` or `unibus-services ... --json`.
    pub fn with_service_catalog(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.services = Some(ServiceIndex::load(path.into()));
        self.generation = self.generation.saturating_add(1);
        self
    }

    /// Load `peers.json` and/or `services.json` from one host-granted directory.
    pub fn with_mycelium_snapshot_dir(mut self, dir: impl AsRef<std::path::Path>) -> Self {
        let dir = dir.as_ref();
        let peers = dir.join("peers.json");
        if peers.is_file() {
            self = self.with_mycelium_peers(peers);
        }
        let services = dir.join("services.json");
        if services.is_file() {
            self = self.with_service_catalog(services);
        }
        self
    }

    /// Add the host-local installed-application inventory.
    ///
    /// This is the one discovery source that is never synced: the inventory is
    /// rebuilt from the bundle index, differs per machine, and is worth nothing
    /// to another host. A Spore cannot add a path or an application; the host
    /// supplies the snapshot.
    pub fn with_applications_snapshot(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.applications = Some(ApplicationIndex::load(path.into()));
        self.generation = self.generation.saturating_add(1);
        self
    }

    /// Add a host-granted Marbles snapshot. The host decides store, company,
    /// project scope and freshness; search never runs `mb` itself.
    pub fn with_marbles(mut self, index: MarblesIndex) -> Self {
        self.marbles = Some(index);
        self.generation = self.generation.saturating_add(1);
        self
    }

    /// Add the current host-attested principal set to search and mentions.
    pub fn with_principals(mut self, principals: Vec<PrincipalCandidate>) -> Self {
        self.principals = principals;
        self.generation = self.generation.saturating_add(1);
        self
    }

    /// Remember usage at `path`. Without this the cache is memory-only, which is
    /// the right default for a short-lived host and for a test.
    pub fn with_mru(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.mru = MruCache::open(path);
        self
    }

    /// The page a launcher shows before anyone types: the host's own usage
    /// memory, answered from the cache without touching a source or the index.
    pub fn recent(&self, limit: u32) -> Vec<api::Hit> {
        let limit = limit.clamp(1, MAX_PAGE) as usize;
        self.mru
            .recent(limit)
            .iter()
            .enumerate()
            .map(|(rank, usage)| hit_from_usage(usage, rank))
            .collect()
    }

    /// Record that a person picked a row. The host calls this where it performs
    /// the launch, so usage counts launches rather than renders.
    pub fn record_use(&mut self, hit: &api::Hit) -> anyhow::Result<()> {
        self.mru.record(usage_from_hit(hit, now_ms()))
    }

    /// Remember launcher preferences at `path`.
    pub fn with_launcher_config(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        let path = path.into();
        self.launcher = LauncherConfig::load(&path);
        self.launcher_path = Some(path);
        self
    }

    pub fn launcher_config(&self) -> &LauncherConfig {
        &self.launcher
    }

    /// Replace the launcher's preferences, clamped to what the host honours and
    /// persisted where they were loaded from.
    pub fn set_launcher_config(
        &mut self,
        config: LauncherConfig,
    ) -> anyhow::Result<LauncherConfig> {
        let config = config.normalized();
        if let Some(path) = &self.launcher_path {
            config.save(path)?;
        }
        self.launcher = config.clone();
        Ok(config)
    }

    /// The surfaces a Spore declares, so a tile can say what it would open. Views
    /// are Spore surfaces, not a kind of their own.
    pub fn spore_surfaces(&self, id: &str) -> Option<Vec<String>> {
        self.spores
            .as_ref()?
            .get(id)
            .map(|entry| entry.surfaces.clone())
    }

    /// The wall a launcher shows at rest: pins in the order a person arranged
    /// them, then what this device actually opens, then — on a first run — what
    /// the sources know. Hidden kinds never appear and no id appears twice.
    pub fn tiles(&self, limit: u32) -> Vec<api::Hit> {
        let limit = limit.clamp(1, MAX_PAGE) as usize;
        let mut tiles: Vec<api::Hit> = Vec::new();
        let mut seen = BTreeSet::new();

        for pin in &self.launcher.pins {
            let Some(hit) = self.hit_by_id(pin) else {
                continue;
            };
            if self.launcher.hides(kind_name(hit.kind)) || !seen.insert(hit.id.clone()) {
                continue;
            }
            tiles.push(hit);
        }
        let mut learned = 0;
        for usage in self.mru.recent(usize::MAX) {
            if self.launcher.hides(&usage.kind) || !seen.insert(usage.id.clone()) {
                continue;
            }
            tiles.push(hit_from_usage(usage, tiles.len()));
            learned += 1;
        }
        // A first run has no history, and a wall holding one pinned tile is not a
        // launcher: fill from what the sources know until something is learned.
        if learned == 0 {
            for hit in self.fallback_tiles() {
                if self.launcher.hides(kind_name(hit.kind)) || !seen.insert(hit.id.clone()) {
                    continue;
                }
                tiles.push(hit);
            }
        }
        tiles.truncate(limit);
        tiles
    }

    /// A pinned id resolved against the sources that keep their entries in
    /// memory. Files and records are deliberately absent: pinning one means
    /// pinning the row a search showed, not spending a stat to redraw a tile.
    fn hit_by_id(&self, id: &str) -> Option<api::Hit> {
        if let Some(applications) = &self.applications
            && let Some(entry) = applications.get(id)
        {
            return Some(application_hit(entry.clone(), 0));
        }
        if let Some(spores) = &self.spores
            && let Some(entry) = spores.get(id)
        {
            return Some(spore_hit(entry.clone(), 0));
        }
        None
    }

    fn fallback_tiles(&self) -> Vec<api::Hit> {
        let mut tiles = Vec::new();
        if let Some(applications) = &self.applications {
            tiles.extend(
                applications
                    .entries()
                    .iter()
                    .take(12)
                    .cloned()
                    .map(|entry| application_hit(entry, 0)),
            );
        }
        if let Some(spores) = &self.spores {
            tiles.extend(
                spores
                    .entries()
                    .iter()
                    .take(8)
                    .cloned()
                    .map(|entry| spore_hit(entry, 0)),
            );
        }
        tiles
    }

    /// Sources that describe an installable world are re-read when that world
    /// moves: a Spore installed while the launcher runs has to appear without a
    /// restart, and a new directory only moves its parent's mtime.
    fn reload_stale(&mut self) {
        let mut moved = false;
        if let Some(spores) = self.spores.as_mut() {
            moved |= spores.reload_if_stale();
        }
        if let Some(applications) = self.applications.as_mut() {
            moved |= applications.reload_if_stale();
        }
        if moved {
            // A page's `generation` promises what produced it; a source that
            // re-read itself is a new generation even without an explicit refresh.
            self.generation = self.generation.saturating_add(1);
        }
    }

    fn sources(&self) -> u32 {
        u32::from(self.vfs.is_some())
            + u32::from(self.records.is_some())
            + u32::from(self.git.is_some())
            + u32::from(self.tgrep.is_some())
            + u32::from(self.spores.is_some())
            + u32::from(self.peers.is_some())
            + u32::from(self.services.is_some())
            + u32::from(self.applications.is_some())
            + u32::from(self.marbles.is_some())
            + u32::from(!self.principals.is_empty())
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
        self.reload_stale();
        let kinds = if query.kinds.is_empty() {
            vec![
                search_api::Kind::File,
                search_api::Kind::RecordEntry,
                search_api::Kind::Repository,
                search_api::Kind::Spore,
                search_api::Kind::Peer,
                search_api::Kind::Service,
                search_api::Kind::Application,
                search_api::Kind::Task,
                search_api::Kind::Principal,
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
        if kinds.contains(&search_api::Kind::Spore) {
            hits.extend(self.spore_hits(&query));
        }
        if kinds.contains(&search_api::Kind::Peer) {
            hits.extend(self.peer_hits(&query));
        }
        if kinds.contains(&search_api::Kind::Service) {
            hits.extend(self.service_hits(&query));
        }
        if kinds.contains(&search_api::Kind::Application) {
            hits.extend(self.application_hits(&query));
        }
        if kinds.contains(&search_api::Kind::Action) {
            hits.extend(self.action_hits(&query));
        }
        if kinds.contains(&search_api::Kind::Task) {
            hits.extend(self.marbles_hits(&query));
        }
        if kinds.contains(&search_api::Kind::Principal) {
            hits.extend(self.principal_hits(&query));
        }
        self.graph.enrich(&mut hits);
        // Usage tilts rows that are otherwise tied. The Spore still owns match
        // policy; this only says that a row someone picks every morning should
        // beat a stranger with the same words in it.
        let now = now_ms();
        for hit in &mut hits {
            hit.score = hit.score.saturating_add(self.mru.boost(&hit.id, now));
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
                    references: Vec::new(),
                    icon: None,
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
        use ::soil::vfs_host::index::Host as _;
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
                references: Vec::new(),
                icon: None,
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
                references: Vec::new(),
                icon: None,
            })
            .collect()
    }

    fn spore_hits(&self, query: &search_api::Query) -> Vec<search_api::Hit> {
        let Some(spores) = &self.spores else {
            return Vec::new();
        };
        spores
            .search(&query.text, MAX_PAGE as usize)
            .into_iter()
            .map(|(entry, score)| spore_hit(entry, score))
            .collect()
    }

    fn peer_hits(&self, query: &search_api::Query) -> Vec<search_api::Hit> {
        let Some(peers) = &self.peers else {
            return Vec::new();
        };
        peers
            .search(&query.text, MAX_PAGE as usize)
            .into_iter()
            .map(|(entry, score)| search_api::Hit {
                kind: search_api::Kind::Peer,
                id: entry.id,
                title: entry.title,
                subtitle: Some(entry.subtitle),
                snippet: entry.snippet,
                score,
                source: "mycelium:peers".into(),
                updated_at_ms: entry.updated_at_ms,
                references: Vec::new(),
                icon: None,
            })
            .collect()
    }

    fn service_hits(&self, query: &search_api::Query) -> Vec<search_api::Hit> {
        let Some(services) = &self.services else {
            return Vec::new();
        };
        services
            .search(&query.text, MAX_PAGE as usize)
            .into_iter()
            .map(|(entry, score)| search_api::Hit {
                kind: search_api::Kind::Service,
                id: entry.id,
                title: entry.title,
                subtitle: Some(entry.subtitle),
                snippet: entry.snippet,
                score,
                source: "mycelium:services".into(),
                updated_at_ms: entry.updated_at_ms,
                references: Vec::new(),
                icon: None,
            })
            .collect()
    }

    fn application_hits(&self, query: &search_api::Query) -> Vec<search_api::Hit> {
        let Some(applications) = &self.applications else {
            return Vec::new();
        };
        applications
            .search(&query.text, MAX_PAGE as usize)
            .into_iter()
            .map(|(entry, score)| application_hit(entry, score))
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
                references: Vec::new(),
                icon: None,
            })
            .collect()
    }

    fn mention_candidates(
        &mut self,
        text: String,
        kinds: Vec<String>,
        limit: u32,
    ) -> Result<Vec<mention_api::Candidate>, search_api::Error> {
        if text.trim().is_empty() || limit == 0 || limit > MAX_MENTIONS {
            return Err(search_api::Error::Invalid);
        }
        let kinds = kinds
            .into_iter()
            .map(|kind| kind.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let allow = |kind: &str| kinds.is_empty() || kinds.contains(kind);
        let mut candidates = Vec::new();

        if ["file", "document", "sheet", "slide"]
            .iter()
            .any(|kind| allow(kind))
        {
            candidates.extend(self.file_mentions(&text, limit)?);
        }

        if let Some(store) = self.records.as_ref() {
            let books: Vec<&str> = self.books.iter().map(String::as_str).collect();
            let records = store
                .lock()
                .expect("records store poisoned")
                .search_books(&books, &text, MAX_PAGE as usize)
                .map_err(|_| search_api::Error::Io)?;
            for (index, record) in records.into_iter().enumerate() {
                let Some(kind) = mention_schema(&record.schema_id) else {
                    continue;
                };
                if !allow(kind) {
                    continue;
                }
                candidates.push(record_mention(record, kind, index));
            }
        }
        if allow("task")
            && let Some(marbles) = &self.marbles
        {
            for (issue, score) in marbles.search(&text, MAX_MENTIONS as usize) {
                candidates.push(mention_api::Candidate {
                    kind: "task".into(),
                    token: marbles.canonical_ref(&issue),
                    label: issue.title,
                    subtitle: Some(format!("{} · {}", issue.project, issue.status)),
                    schema: None,
                    format: None,
                    score,
                    visibility: "team".into(),
                });
            }
        }
        if allow("principal") {
            let needle = text.to_ascii_lowercase();
            for (index, principal) in self.principals.iter().enumerate() {
                let haystack = format!(
                    "{} {} {} {}",
                    principal.actor,
                    principal.display_name.as_deref().unwrap_or_default(),
                    principal.teams.join(" "),
                    principal.roles.join(" ")
                )
                .to_ascii_lowercase();
                if !haystack.contains(&needle) {
                    continue;
                }
                candidates.push(mention_api::Candidate {
                    kind: "principal".into(),
                    token: format!("principal:{}", principal.actor),
                    label: principal
                        .display_name
                        .clone()
                        .unwrap_or_else(|| principal.actor.clone()),
                    subtitle: Some(
                        principal
                            .teams
                            .iter()
                            .chain(&principal.roles)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                    schema: None,
                    format: None,
                    score: 950_000u32.saturating_sub(index as u32),
                    visibility: principal.visibility.clone(),
                });
            }
        }
        candidates.retain(|candidate| allow(&candidate.kind));

        Ok(finish_mentions(candidates, limit))
    }

    fn file_mentions(
        &mut self,
        text: &str,
        limit: u32,
    ) -> Result<Vec<mention_api::Candidate>, search_api::Error> {
        let Some(vfs) = self.vfs.as_mut() else {
            return Ok(Vec::new());
        };
        use ::soil::vfs_host::index::Host as _;
        let page = vfs
            .search(index::Query {
                text: text.to_string(),
                mode: Some(index::Mode::Fuzzy),
                parent: None,
                media_type_prefix: None,
                offset: 0,
                limit: limit.min(MAX_MENTIONS),
            })
            .map_err(index_error)?;
        Ok(page
            .hits
            .into_iter()
            .map(|hit| {
                let (kind, format) = file_mention_class(&hit.path);
                let label = file_mention_label(&hit.path, kind);
                let subtitle = hit
                    .path
                    .rsplit_once('/')
                    .map(|(parent, _)| parent.to_string());
                mention_api::Candidate {
                    kind: kind.into(),
                    token: format!("file:{}", hit.path),
                    label,
                    subtitle,
                    schema: None,
                    format,
                    score: hit.score,
                    visibility: "workspace".into(),
                }
            })
            .collect())
    }

    fn marbles_hits(&self, query: &search_api::Query) -> Vec<search_api::Hit> {
        let Some(marbles) = &self.marbles else {
            return Vec::new();
        };
        marbles
            .search(&query.text, MAX_PAGE as usize)
            .into_iter()
            .map(|(issue, score)| search_api::Hit {
                kind: search_api::Kind::Task,
                id: marbles.canonical_ref(&issue),
                title: issue.title,
                subtitle: Some(format!(
                    "{} · {} · p{}",
                    issue.project, issue.status, issue.priority
                )),
                snippet: (!issue.description.is_empty()).then_some(issue.description),
                score,
                source: "marbles".into(),
                updated_at_ms: Some(issue.updated_at.max(0) as u64 * 1000),
                references: Vec::new(),
                icon: None,
            })
            .collect()
    }

    fn principal_hits(&self, query: &search_api::Query) -> Vec<search_api::Hit> {
        let needle = query.text.to_ascii_lowercase();
        self.principals
            .iter()
            .filter(|principal| {
                principal.actor.to_ascii_lowercase().contains(&needle)
                    || principal
                        .display_name
                        .as_deref()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .contains(&needle)
                    || principal
                        .teams
                        .iter()
                        .chain(&principal.roles)
                        .any(|value| value.to_ascii_lowercase().contains(&needle))
            })
            .enumerate()
            .map(|(index, principal)| search_api::Hit {
                kind: search_api::Kind::Principal,
                id: format!("principal:{}", principal.actor),
                title: principal
                    .display_name
                    .clone()
                    .unwrap_or_else(|| principal.actor.clone()),
                subtitle: Some(
                    principal
                        .teams
                        .iter()
                        .chain(&principal.roles)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" · "),
                ),
                snippet: None,
                score: 800_000u32.saturating_sub(index as u32),
                source: "principal".into(),
                updated_at_ms: None,
                references: Vec::new(),
                icon: None,
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

fn file_mention_class(path: &str) -> (&'static str, Option<String>) {
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some(
            "md" | "markdown" | "mdx" | "txt" | "rst" | "adoc" | "org" | "tex" | "doc" | "docx"
            | "odt" | "rtf" | "epub" | "html",
        ) => ("document", extension),
        Some("pdf") => ("document", extension),
        Some("xls" | "xlsx" | "xlsm" | "ods" | "csv" | "tsv" | "numbers") => ("sheet", extension),
        Some("ppt" | "pptx" | "pptm" | "odp" | "key") => ("slide", extension),
        _ => ("file", extension),
    }
}

fn file_mention_label(path: &str, kind: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if kind == "file" {
        return name.to_string();
    }
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    stem.replace(['-', '_'], " ")
}

fn mention_schema(schema: &str) -> Option<&'static str> {
    match schema {
        "fungos.crm/company@1" => Some("company"),
        "fungos.crm/account@1" => Some("account"),
        "fungos.crm/contact@1" | "fungos.contacts/contact" => Some("person"),
        "fungos.crm/project@1" => Some("project"),
        "fungos.crm/opportunity@1" => Some("opportunity"),
        "fungos.crm/activity@1" => Some("activity"),
        "fungos.crm/call@1" => Some("call"),
        "fungos.mail/thread@1" => Some("thread"),
        "fungos.mail/message@1" => Some("message"),
        "fungos.doc/document" | "fungos.doc/document@1" | "fungos.docs/document@1" => {
            Some("document")
        }
        "fungos.note/note@1" | "fungos.notes/note@1" => Some("document"),
        "fungos.sheet/sheet@1" | "fungos.sheets/sheet@1" => Some("sheet"),
        "fungos.slide/slide@1" | "fungos.slides/slide@1" => Some("slide"),
        _ => None,
    }
}

fn record_mention(record: Record, kind: &str, index: usize) -> mention_api::Candidate {
    let fields = &record.fields;
    let label = ["display_name", "name", "title", "subject", "headline"]
        .iter()
        .find_map(|key| fields.get(key).and_then(serde_json::Value::as_str))
        .filter(|value| !value.is_empty())
        .unwrap_or(&record.entity_id)
        .to_string();
    mention_api::Candidate {
        kind: kind.to_string(),
        token: format!("record:{}#{}", record.book_id, record.entity_id),
        label,
        subtitle: mention_subtitle(kind, fields),
        schema: Some(record.schema_id.clone()),
        format: text_field(fields, "format")
            .or_else(|| text_field(fields, "file_format"))
            .or_else(|| text_field(fields, "media_type")),
        score: 900_000u32.saturating_sub(index as u32),
        visibility: fields
            .get("visibility")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("private")
            .to_string(),
    }
}

fn mention_subtitle(kind: &str, fields: &serde_json::Value) -> Option<String> {
    match kind {
        "person" => text_field(fields, "title")
            .or_else(|| text_field(fields, "account_name"))
            .or_else(|| text_field(fields, "contact_token")),
        "account" | "company" => fields
            .get("domains")
            .and_then(serde_json::Value::as_array)
            .and_then(|domains| domains.first())
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| text_field(fields, "industry")),
        "project" => text_field(fields, "status").or_else(|| text_field(fields, "kind")),
        "task" => text_field(fields, "status").or_else(|| text_field(fields, "due_at")),
        "thread" => text_field(fields, "channel"),
        "message" => text_field(fields, "sender_name").or_else(|| text_field(fields, "sender")),
        "call" => text_field(fields, "direction").or_else(|| text_field(fields, "status")),
        _ => None,
    }
}

fn text_field(fields: &serde_json::Value, key: &str) -> Option<String> {
    fields
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn finish_mentions(
    mut candidates: Vec<mention_api::Candidate>,
    limit: u32,
) -> Vec<mention_api::Candidate> {
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.label.cmp(&right.label))
    });
    let mut seen = BTreeSet::new();
    candidates.retain(|candidate| seen.insert(candidate.token.clone()));
    candidates.truncate(limit as usize);
    candidates
}

fn record_hit(record: Record, needle: &str, index: usize) -> search_api::Hit {
    let fields = record.fields;
    let title = ["display_name", "name", "title", "subject", "headline"]
        .iter()
        .find_map(|key| fields.get(key).and_then(serde_json::Value::as_str))
        .filter(|value| !value.is_empty())
        .unwrap_or(&record.entity_id)
        .to_string();
    let body = fields.to_string().to_lowercase();
    let snippet = body.find(needle).map(|position| {
        let mut start = position.saturating_sub(40);
        while !body.is_char_boundary(start) {
            start = start.saturating_sub(1);
        }
        let mut end = (position + needle.len() + 80).min(body.len());
        while end < body.len() && !body.is_char_boundary(end) {
            end += 1;
        }
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
        references: Vec::new(),
        icon: None,
    }
}

/// One application row, wherever it came from: a query or a pinned id. Two
/// mappings would drift, and a pinned tile would then look unlike a searched one.
fn application_hit(entry: ApplicationEntry, score: u32) -> api::Hit {
    api::Hit {
        kind: api::Kind::Application,
        id: entry.id,
        title: entry.title,
        subtitle: Some(entry.subtitle),
        snippet: None,
        score,
        source: "host:applications".into(),
        updated_at_ms: None,
        references: Vec::new(),
        icon: entry.icon,
    }
}

fn spore_hit(entry: SporeEntry, score: u32) -> api::Hit {
    api::Hit {
        kind: api::Kind::Spore,
        id: entry.id,
        title: entry.name,
        subtitle: Some(format!("{} · {}", entry.version, entry.target)),
        snippet: (!entry.summary.is_empty()).then_some(entry.summary),
        score,
        source: format!("spore:{}", entry.root),
        updated_at_ms: None,
        references: Vec::new(),
        icon: None,
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
        self.graph = self
            .records
            .as_ref()
            .map(|store| {
                GraphIndex::build(&store.lock().expect("records store poisoned"), &self.books)
            })
            .unwrap_or_default();
        if let Some(git) = self.git.as_mut() {
            git.refresh();
        }
        if let Some(spores) = self.spores.as_mut() {
            spores.refresh();
        }
        if let Some(applications) = self.applications.as_mut() {
            applications.refresh();
        }
        if let Some(peers) = self.peers.as_mut() {
            peers.refresh();
        }
        if let Some(services) = self.services.as_mut() {
            services.refresh();
        }
        self.generation = self.generation.saturating_add(1);
        Ok(self.snapshot())
    }
}

impl mention_api::Host for SearchService {
    fn suggest(
        &mut self,
        text: String,
        kinds: Vec<String>,
        limit: u32,
    ) -> Result<Vec<mention_api::Candidate>, search_api::Error> {
        SearchService::mention_candidates(self, text, kinds, limit)
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: search_api::Host + mention_api::Host + 'static,
{
    search_api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    mention_api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soil::search::mentions::Host as _;
    use crate::soil::search::search::Host as _;
    use fungos_vfs::{Expect, Vfs};

    /// A store inside the root's own state directory: it dies with the tempdir,
    /// and no other test (or the operator's real store) can see it.
    fn open_owned(root: &std::path::Path) -> Vfs {
        Vfs::open_with(
            root,
            fungos_vfs::NodeRole::Full,
            Some(&root.join(".fungos/store")),
        )
        .unwrap()
    }

    fn seed_vfs(root: &std::path::Path) {
        let mut vfs = open_owned(root);
        vfs.put(
            "notes/greece.txt",
            b"a greece trip",
            "text/plain",
            Expect::Any,
        )
        .unwrap();
        vfs.put("photos/paris.jpg", b"jpeg", "image/jpeg", Expect::Any)
            .unwrap();
    }

    fn seed_office_files(root: &std::path::Path) {
        let mut vfs = open_owned(root);
        vfs.put("docs/brief.pdf", b"pdf", "application/pdf", Expect::Any)
            .unwrap();
        vfs.put(
            "sheets/budget.xlsx",
            b"xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Expect::Any,
        )
        .unwrap();
        vfs.put(
            "slides/kickoff.pptx",
            b"pptx",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            Expect::Any,
        )
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
        let vfs = VfsHost::open_with_role_and_store(
            dir.path(),
            fungos_vfs::NodeRole::Full,
            Some(&dir.path().join(".fungos/store")),
        )
        .unwrap();
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
    fn searches_opt_in_spore_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let spore = dir.path().join("markdown");
        std::fs::create_dir_all(&spore).unwrap();
        std::fs::write(
            spore.join("spore.toml"),
            r#"schema = 1
[spore]
id = "dev.fungos.markdown"
name = "markdown"
version = "0.1.0"
license = "MIT"
summary = "Markdown reader with a Canvas UI"
[engine]
soil = ">=0.1"
target = "wasm32-wasip2"
[module]
path = "module.wasm"
[ui]
entry = "ui/editor.lua"
[launch]
default = "canvas"
surfaces = ["canvas"]
[grants]
"soil:canvas" = "present the editor"
"#,
        )
        .unwrap();
        let mut host =
            SearchService::new(None, None, vec![]).with_spore_roots(vec![dir.path().to_path_buf()]);
        let page = host
            .search(search_api::Query {
                text: "markdown".into(),
                kinds: vec![search_api::Kind::Spore],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].kind, search_api::Kind::Spore);
        assert_eq!(page.hits[0].title, "markdown");
        assert!(page.hits[0].id.starts_with("dev.fungos."));
    }

    fn install_spore(root: &std::path::Path, package: &str, id: &str, name: &str) {
        let spore = root.join(package);
        std::fs::create_dir_all(&spore).unwrap();
        std::fs::write(
            spore.join("spore.toml"),
            format!(
                r#"schema = 1
[spore]
id = "{id}"
name = "{name}"
version = "0.1.0"
license = "MIT"
summary = "{name} reader with a Canvas UI"
[engine]
soil = ">=0.1"
target = "wasm32-wasip2"
[module]
path = "module.wasm"
[ui]
entry = "ui/editor.lua"
[launch]
default = "canvas"
surfaces = ["canvas"]
[grants]
"soil:canvas" = "present the editor"
"#
            ),
        )
        .unwrap();
    }

    fn application_row(id: &str, title: &str) -> search_api::Hit {
        search_api::Hit {
            kind: search_api::Kind::Application,
            id: id.into(),
            title: title.into(),
            subtitle: Some("Applications".into()),
            snippet: None,
            score: 0,
            source: "host:applications".into(),
            updated_at_ms: None,
            references: Vec::new(),
            icon: None,
        }
    }

    /// The launcher's first frame must not wait on a source: usage memory answers
    /// with an empty service, and it is on disk for the next summon.
    #[test]
    fn recents_answer_before_any_source_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/mru.json");
        let mut host = SearchService::new(None, None, vec![]).with_mru(&path);
        assert!(host.recent(8).is_empty());

        host.record_use(&application_row("com.apple.Safari", "Safari"))
            .unwrap();
        let recent = host.recent(8);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].title, "Safari");
        assert_eq!(recent[0].kind, search_api::Kind::Application);
        assert_eq!(host.state().unwrap().sources, 0);

        let reopened = SearchService::new(None, None, vec![]).with_mru(&path);
        assert_eq!(reopened.recent(8)[0].id, "com.apple.Safari");
    }

    /// Two applications match equally; the one someone has picked before wins,
    /// because the host remembers launches and the source does not.
    #[test]
    fn a_remembered_row_beats_an_equally_good_stranger() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("applications.json");
        std::fs::write(
            &snapshot,
            serde_json::json!({"applications": [
                {"bundle_id": "com.example.astro", "name": "Astro", "subtitle": "Utilities"},
                {"bundle_id": "com.example.astral", "name": "Astral", "subtitle": "Utilities"},
            ]})
            .to_string(),
        )
        .unwrap();
        let mut host = SearchService::new(None, None, vec![]).with_applications_snapshot(&snapshot);
        let query = || search_api::Query {
            text: "astr".into(),
            kinds: vec![search_api::Kind::Application],
            mode: None,
            offset: 0,
            limit: 10,
        };

        let before = host.search(query()).unwrap();
        assert_eq!(before.hits.len(), 2);
        assert_eq!(
            before.hits[0].id, "com.example.astral",
            "title breaks the tie"
        );

        let picked = before
            .hits
            .iter()
            .find(|hit| hit.id == "com.example.astro")
            .unwrap()
            .clone();
        host.record_use(&picked).unwrap();

        let after = host.search(query()).unwrap();
        assert_eq!(after.hits[0].id, "com.example.astro");
        assert!(after.hits[0].score > after.hits[1].score);
    }

    /// Installing a Spore while the launcher runs has to show up without a
    /// restart: the new directory moves the root's mtime, which is all the
    /// service stats before paying for a walk.
    #[test]
    fn a_spore_installed_while_running_appears_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut host =
            SearchService::new(None, None, vec![]).with_spore_roots(vec![dir.path().to_path_buf()]);
        let query = || search_api::Query {
            text: "markdown".into(),
            kinds: vec![search_api::Kind::Spore],
            mode: None,
            offset: 0,
            limit: 10,
        };
        let before = host.search(query()).unwrap();
        assert!(before.hits.is_empty());

        install_spore(dir.path(), "markdown", "dev.fungos.markdown", "markdown");

        let after = host.search(query()).unwrap();
        assert_eq!(after.hits.len(), 1);
        assert_eq!(after.hits[0].id, "dev.fungos.markdown");
        assert!(
            after.generation > before.generation,
            "a reloaded source is a new generation"
        );
    }

    #[test]
    fn searches_mycelium_peer_and_service_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let peers = dir.path().join("peers.json");
        std::fs::write(
            &peers,
            serde_json::json!([{
                "origin": "studio-origin",
                "last_seen": 1_700_000_000u64,
                "hello": {
                    "node_id": "studio-origin",
                    "hostname": "studio",
                    "site": "lab",
                    "platform": "Darwin",
                    "architecture": "aarch64",
                    "daemon_version": "0.1.0",
                    "capabilities": ["unibus-viewer", "ssh"]
                }
            }])
            .to_string(),
        )
        .unwrap();
        let services = dir.path().join("services.json");
        std::fs::write(
            &services,
            serde_json::json!({
                "schema_version": 2,
                "observations": [{
                    "schema_version": 2,
                    "service_id": "unibus/viewer",
                    "observed_at": 1_700_000_000u64,
                    "expires_at": 1_700_000_300u64,
                    "value": {
                        "kind": "unibus",
                        "endpoints": ["http://studio.local:7411"],
                        "protocols": ["dcp"],
                        "formats": [],
                        "attributes": {"node_label": "studio"}
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();
        let mut host = SearchService::new(None, None, vec![])
            .with_mycelium_peers(&peers)
            .with_service_catalog(&services);

        let page = host
            .search(search_api::Query {
                text: "studio".into(),
                kinds: vec![search_api::Kind::Peer],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].kind, search_api::Kind::Peer);
        assert_eq!(page.hits[0].title, "studio");

        let page = host
            .search(search_api::Query {
                text: "unibus".into(),
                kinds: vec![search_api::Kind::Service],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].kind, search_api::Kind::Service);
        assert_eq!(page.hits[0].id, "unibus/viewer");
    }

    #[test]
    fn a_host_local_application_inventory_is_found_and_carries_its_icon() {
        let dir = tempfile::tempdir().unwrap();
        let applications = dir.path().join("applications.json");
        std::fs::write(
            &applications,
            serde_json::json!({
                "applications": [
                    {
                        "bundle_id": "com.apple.Safari",
                        "name": "Safari",
                        "subtitle": "com.apple.Safari",
                        "icon": "asset:sha256:aa11"
                    },
                    { "bundle_id": "com.tinyspeck.slackmacgap", "name": "Slack" }
                ]
            })
            .to_string(),
        )
        .unwrap();
        let mut host =
            SearchService::new(None, None, vec![]).with_applications_snapshot(&applications);

        let page = host
            .search(search_api::Query {
                text: "safari".into(),
                kinds: vec![search_api::Kind::Application],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].kind, search_api::Kind::Application);
        // The locator is a bundle id, not a path the Spore could steer.
        assert_eq!(page.hits[0].id, "com.apple.Safari");
        assert_eq!(page.hits[0].source, "host:applications");
        assert_eq!(page.hits[0].icon.as_deref(), Some("asset:sha256:aa11"));

        // An application with no icon is still a row: the renderer falls back to
        // a monogram, it does not drop the hit.
        let page = host
            .search(search_api::Query {
                text: "slack".into(),
                kinds: vec![search_api::Kind::Application],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].icon, None);

        // The inventory is host-granted, not ambient: without the snapshot the
        // kind yields nothing rather than inventing a list.
        let mut bare = SearchService::new(None, None, vec![]);
        let page = bare
            .search(search_api::Query {
                text: "safari".into(),
                kinds: vec![search_api::Kind::Application],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert!(page.hits.is_empty());
    }

    #[test]
    fn merges_files_and_records() {
        let dir = tempfile::tempdir().unwrap();
        seed_vfs(dir.path());
        let store = seed_records(&dir.path().join("records"));
        let vfs = VfsHost::open_with_role_and_store(
            dir.path(),
            fungos_vfs::NodeRole::Full,
            Some(&dir.path().join(".fungos/store")),
        )
        .unwrap();
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
    fn record_hits_carry_graph_links_and_references() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = seed_records(&dir.path().join("records"));
        let book = "personal/avery/contacts";
        store
            .put_record(
                book,
                "soil",
                "fungos.project/project",
                1,
                serde_json::json!({
                    "title": "Soil",
                    "status": "active"
                }),
            )
            .unwrap();
        store
            .put_record(
                book,
                "link-soil-repo",
                crate::graph::LINK_SCHEMA,
                1,
                serde_json::json!({
                    "from": "soil",
                    "rel": "references",
                    "to": "repository:mycelium-linux/soil",
                    "title": "Soil repository",
                    "role": "primary"
                }),
            )
            .unwrap();
        store
            .put_record(
                book,
                "ref-soil-crate",
                crate::graph::REFERENCE_SCHEMA,
                1,
                serde_json::json!({
                    "node": "soil",
                    "scheme": "purl",
                    "value": "pkg:cargo/soil@0.1.0",
                    "title": "Soil crate"
                }),
            )
            .unwrap();

        let mut host = SearchService::new(None, Some(store), vec![book.into()]);
        let page = host
            .search(search_api::Query {
                text: "Soil".into(),
                kinds: vec![search_api::Kind::RecordEntry],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        let project = page
            .hits
            .iter()
            .find(|hit| hit.id == format!("{book}/soil"))
            .expect("project hit");
        assert!(project.references.iter().any(|reference| {
            reference.kind == "repository"
                && reference.id == "repository:mycelium-linux/soil"
                && reference.rel.as_deref() == Some("references")
        }));
        assert!(project.references.iter().any(|reference| {
            reference.kind == "purl" && reference.id == "pkg:cargo/soil@0.1.0"
        }));
    }

    #[test]
    fn scope_filters_and_refresh_generation() {
        let dir = tempfile::tempdir().unwrap();
        seed_vfs(dir.path());
        let store = seed_records(&dir.path().join("records"));
        let vfs = VfsHost::open_with_role_and_store(
            dir.path(),
            fungos_vfs::NodeRole::Full,
            Some(&dir.path().join(".fungos/store")),
        )
        .unwrap();
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

    #[test]
    fn mention_suggestions_are_typed_and_schema_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let book = "personal/avery/crm";
        let mut store = Store::open(dir.path(), &dir.path().join("device_id")).unwrap();
        store
            .put_record(
                book,
                "a-acme",
                "fungos.crm/account@1",
                1,
                serde_json::json!({
                    "name": "Acme Robotics",
                    "domains": ["acme.example"],
                    "visibility": "team"
                }),
            )
            .unwrap();
        store
            .put_record(
                book,
                "c-ada",
                "fungos.crm/contact@1",
                1,
                serde_json::json!({
                    "name": "Ada Lovelace",
                    "title": "CTO",
                    "contact_token": "t-ada",
                    "visibility": "team"
                }),
            )
            .unwrap();
        store
            .put_record(
                book,
                "p-acme-pilot",
                "fungos.crm/project@1",
                1,
                serde_json::json!({
                    "name": "Acme pilot",
                    "status": "active",
                    "visibility": "team"
                }),
            )
            .unwrap();
        store
            .put_record(
                book,
                "doc-arch",
                "fungos.doc/document",
                1,
                serde_json::json!({
                    "title": "Architecture",
                    "visibility": "team"
                }),
            )
            .unwrap();
        store
            .put_record(
                book,
                "x-not-mentionable",
                "dev.fungos/unknown@1",
                1,
                serde_json::json!({"name": "Ada Mystery"}),
            )
            .unwrap();

        let mut host = SearchService::new(None, Some(store), vec![book.into()]);
        let all = host.suggest("ada".into(), vec![], 10).unwrap();
        let person = all
            .iter()
            .find(|candidate| candidate.kind == "person")
            .expect("person candidate");
        assert_eq!(person.token, format!("record:{book}#c-ada"));
        assert_eq!(person.schema.as_deref(), Some("fungos.crm/contact@1"));
        assert_eq!(person.visibility, "team");
        assert!(all.iter().all(|candidate| candidate.kind != "record"));
        assert!(
            all.iter()
                .all(|candidate| !candidate.token.contains("x-not-mentionable"))
        );

        let accounts = host
            .suggest("acme".into(), vec!["account".into()], 10)
            .unwrap();
        assert!(accounts.iter().all(|candidate| candidate.kind == "account"));
        assert_eq!(accounts[0].token, format!("record:{book}#a-acme"));

        let documents = host
            .suggest("architecture".into(), vec!["document".into()], 10)
            .unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].token, format!("record:{book}#doc-arch"));
        assert_eq!(documents[0].schema.as_deref(), Some("fungos.doc/document"));
    }

    #[test]
    fn mention_suggestions_include_workspace_documents_and_files() {
        let dir = tempfile::tempdir().unwrap();
        seed_vfs(dir.path());
        seed_office_files(dir.path());
        let records = tempfile::tempdir().unwrap();
        let mut host = SearchService::open(dir.path(), records.path(), vec![]).unwrap();
        let documents = host
            .suggest("greece".into(), vec!["document".into()], 5)
            .unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].kind, "document");
        assert_eq!(documents[0].token, "file:notes/greece.txt");
        assert_eq!(documents[0].label, "greece");
        assert_eq!(documents[0].format.as_deref(), Some("txt"));
        assert_eq!(documents[0].visibility, "workspace");

        let pdfs = host
            .suggest("brief".into(), vec!["document".into()], 5)
            .unwrap();
        assert_eq!(pdfs.len(), 1);
        assert_eq!(pdfs[0].kind, "document");
        assert_eq!(pdfs[0].format.as_deref(), Some("pdf"));

        let sheets = host
            .suggest("budget".into(), vec!["sheet".into()], 5)
            .unwrap();
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].kind, "sheet");
        assert_eq!(sheets[0].format.as_deref(), Some("xlsx"));

        let slides = host
            .suggest("kickoff".into(), vec!["slide".into()], 5)
            .unwrap();
        assert_eq!(slides.len(), 1);
        assert_eq!(slides[0].kind, "slide");
        assert_eq!(slides[0].format.as_deref(), Some("pptx"));

        let files = host
            .suggest("paris".into(), vec!["file".into()], 5)
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, "file");
        assert_eq!(files[0].format.as_deref(), Some("jpg"));
        assert_eq!(files[0].token, "file:photos/paris.jpg");
    }

    #[test]
    fn one_search_includes_marbles_and_principals() {
        let marbles = MarblesIndex::new(
            "store",
            "fpl",
            vec![soil_marbles::Issue {
                id: "ACME-42".into(),
                title: "Fix SSO".into(),
                description: "The SAML callback fails".into(),
                status: "open".into(),
                priority: 1,
                project: "acme".into(),
                updated_at: 1_700_000_000,
                ..soil_marbles::Issue::default()
            }],
        );
        let mut host = SearchService::new(None, None, vec![])
            .with_marbles(marbles)
            .with_principals(vec![PrincipalCandidate {
                actor: "oidc:https://identity.example#user-42".into(),
                display_name: Some("Avery".into()),
                teams: vec!["fpl".into()],
                roles: vec!["operator".into()],
                visibility: "team".into(),
            }]);

        let tasks = host
            .search(search_api::Query {
                text: "sso".into(),
                kinds: vec![search_api::Kind::Task],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(tasks.hits.len(), 1);
        assert_eq!(tasks.hits[0].id, "marbles://store/fpl/acme/ACME-42");

        let principals = host
            .search(search_api::Query {
                text: "avery".into(),
                kinds: vec![search_api::Kind::Principal],
                mode: None,
                offset: 0,
                limit: 10,
            })
            .unwrap();
        assert_eq!(principals.hits.len(), 1);
        assert_eq!(
            principals.hits[0].id,
            "principal:oidc:https://identity.example#user-42"
        );

        let task_mentions = host.suggest("sso".into(), vec!["task".into()], 10).unwrap();
        assert_eq!(task_mentions[0].kind, "task");
        assert_eq!(task_mentions[0].token, "marbles://store/fpl/acme/ACME-42");

        let principal_mentions = host
            .suggest("avery".into(), vec!["principal".into()], 10)
            .unwrap();
        assert_eq!(principal_mentions[0].kind, "principal");
        assert_eq!(
            principal_mentions[0].token,
            "principal:oidc:https://identity.example#user-42"
        );
    }
}
