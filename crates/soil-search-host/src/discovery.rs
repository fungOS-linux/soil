//! Opt-in local discovery indexes for Spores, Mycelium peers, and service
//! catalogs.
//!
//! These are derived indexes over host-granted snapshots or directories. They
//! are not authority: a hit names a local package id, peer origin, or service
//! id. Opening or acting on a hit remains the owning capability's decision.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;
use soil_spore::Manifest;

const MAX_SPORES: usize = 2048;
const MAX_DISCOVERY_DEPTH: usize = 6;
const SKIP: &[&str] = &[
    "target",
    "node_modules",
    ".cache",
    "dist",
    "build",
    "vendor",
    ".worktrees",
    ".git",
];

#[derive(Clone, Debug)]
pub struct SporeEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub summary: String,
    pub target: String,
    pub surfaces: Vec<String>,
    pub grants: Vec<String>,
    pub root: String,
}

impl SporeEntry {
    fn searchable(&self) -> String {
        format!(
            "{} {} {} {} {} {} {}",
            self.id,
            self.name,
            self.version,
            self.summary,
            self.target,
            self.surfaces.join(" "),
            self.grants.join(" ")
        )
        .to_lowercase()
    }
}

#[derive(Default)]
pub struct SporeIndex {
    roots: Vec<PathBuf>,
    entries: Vec<SporeEntry>,
}

impl SporeIndex {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let mut index = Self {
            roots,
            entries: Vec::new(),
        };
        index.refresh();
        index
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        let mut seen = BTreeSet::new();
        for root in &self.roots {
            let mut found = Vec::new();
            discover_spores(root, 0, &mut found);
            for entry in found {
                if seen.insert(entry.id.clone()) {
                    self.entries.push(entry);
                }
            }
        }
        self.entries.sort_by(|a, b| a.id.cmp(&b.id));
        self.entries.truncate(MAX_SPORES);
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(SporeEntry, u32)> {
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        for entry in &self.entries {
            let text = entry.searchable();
            if !text.contains(&needle) {
                continue;
            }
            let score = if entry.id.to_lowercase() == needle || entry.name.to_lowercase() == needle
            {
                990_000
            } else if entry.id.to_lowercase().contains(&needle)
                || entry.name.to_lowercase().contains(&needle)
            {
                970_000
            } else {
                920_000
            };
            hits.push((entry.clone(), score));
        }
        hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
        hits.truncate(limit);
        hits
    }
}

fn discover_spores(root: &Path, depth: usize, entries: &mut Vec<SporeEntry>) {
    if depth > MAX_DISCOVERY_DEPTH || entries.len() >= MAX_SPORES {
        return;
    }
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return;
    }
    let manifest = root.join("spore.toml");
    if manifest.is_file()
        && let Ok(manifest) = Manifest::load(root)
    {
        entries.push(SporeEntry {
            id: manifest.spore.id,
            name: manifest.spore.name,
            version: manifest.spore.version,
            summary: manifest.spore.summary,
            target: manifest.engine.target,
            surfaces: manifest.launch.surfaces,
            grants: manifest.grants.into_keys().collect(),
            root: root.display().to_string(),
        });
    }
    let Ok(read_dir) = fs::read_dir(root) else {
        return;
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIP.contains(&name.as_ref()) || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            discover_spores(&path, depth + 1, entries);
            if entries.len() >= MAX_SPORES {
                return;
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PeerEntry {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub snippet: Option<String>,
    pub updated_at_ms: Option<u64>,
}

impl PeerEntry {
    fn searchable(&self) -> String {
        format!(
            "{} {} {} {}",
            self.id,
            self.title,
            self.subtitle,
            self.snippet.as_deref().unwrap_or_default()
        )
        .to_lowercase()
    }
}

#[derive(Default)]
pub struct PeerIndex {
    path: Option<PathBuf>,
    entries: Vec<PeerEntry>,
}

impl PeerIndex {
    pub fn load(path: PathBuf) -> Self {
        let mut index = Self {
            path: Some(path),
            entries: Vec::new(),
        };
        index.refresh();
        index
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        let Some(path) = &self.path else {
            return;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        for peer in array_field(&value, "peers") {
            let hello = &peer["hello"];
            let origin = string_field(peer, "origin")
                .or_else(|| string_field(hello, "node_id"))
                .unwrap_or_else(|| "unknown-peer".into());
            let hostname = string_field(hello, "hostname")
                .or_else(|| string_field(peer, "hostname"))
                .unwrap_or_else(|| origin.clone());
            let site = string_field(hello, "site").unwrap_or_default();
            let platform = string_field(hello, "platform").unwrap_or_default();
            let architecture = string_field(hello, "architecture").unwrap_or_default();
            let daemon = string_field(hello, "daemon_version").unwrap_or_default();
            let capabilities = string_array(&hello["capabilities"]);
            let subtitle = [
                site.as_str(),
                platform.as_str(),
                architecture.as_str(),
                daemon.as_str(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            let last_seen = peer["last_seen"].as_u64();
            self.entries.push(PeerEntry {
                id: origin,
                title: hostname,
                subtitle: if subtitle.is_empty() {
                    "peer".into()
                } else {
                    subtitle
                },
                snippet: (!capabilities.is_empty()).then(|| capabilities.join(" · ")),
                updated_at_ms: last_seen.map(|seconds| seconds.saturating_mul(1000)),
            });
        }
        self.entries.sort_by(|a, b| a.id.cmp(&b.id));
        self.entries.truncate(MAX_SPORES);
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(PeerEntry, u32)> {
        search_entries(&self.entries, query, limit, 960_000)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ServiceEntry {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub snippet: Option<String>,
    pub updated_at_ms: Option<u64>,
}

impl ServiceEntry {
    fn searchable(&self) -> String {
        format!(
            "{} {} {} {}",
            self.id,
            self.title,
            self.subtitle,
            self.snippet.as_deref().unwrap_or_default()
        )
        .to_lowercase()
    }
}

#[derive(Default)]
pub struct ServiceIndex {
    path: Option<PathBuf>,
    entries: Vec<ServiceEntry>,
}

impl ServiceIndex {
    pub fn load(path: PathBuf) -> Self {
        let mut index = Self {
            path: Some(path),
            entries: Vec::new(),
        };
        index.refresh();
        index
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        let Some(path) = &self.path else {
            return;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        for observation in array_field(&value, "observations") {
            let id = string_field(observation, "service_id")
                .or_else(|| string_field(&observation["service_id"], "0"))
                .unwrap_or_else(|| "unknown-service".into());
            let value = &observation["value"];
            let kind = string_field(value, "kind").unwrap_or_default();
            let attributes = value["attributes"].as_object();
            let node = attributes
                .and_then(|attributes| attributes.get("node_label"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let label = attributes
                .and_then(|attributes| attributes.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let endpoints = string_array(&value["endpoints"]);
            let protocols = string_array(&value["protocols"]);
            let formats = string_array(&value["formats"]);
            let mut snippets = Vec::new();
            if !endpoints.is_empty() {
                snippets.push(endpoints.join(" "));
            }
            if !protocols.is_empty() {
                snippets.push(protocols.join(" "));
            }
            if !formats.is_empty() {
                snippets.push(formats.join(" "));
            }
            if let Some(attributes) = attributes {
                for (key, value) in attributes {
                    snippets.push(format!("{key}={value}"));
                }
            }
            let subtitle = [kind.as_str(), node.as_deref().unwrap_or_default()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            self.entries.push(ServiceEntry {
                title: label.unwrap_or_else(|| id.clone()),
                id,
                subtitle: if subtitle.is_empty() {
                    "service".into()
                } else {
                    subtitle
                },
                snippet: (!snippets.is_empty()).then(|| snippets.join(" · ")),
                updated_at_ms: observation["observed_at"]
                    .as_u64()
                    .map(|seconds| seconds.saturating_mul(1000)),
            });
        }
        self.entries.sort_by(|a, b| a.id.cmp(&b.id));
        self.entries.truncate(MAX_SPORES);
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(ServiceEntry, u32)> {
        search_entries(&self.entries, query, limit, 950_000)
    }
}

fn search_entries<T>(entries: &[T], query: &str, limit: usize, base_score: u32) -> Vec<(T, u32)>
where
    T: Searchable + Clone,
{
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    for entry in entries {
        if !entry.searchable().contains(&needle) {
            continue;
        }
        let score = if entry.id().to_lowercase() == needle || entry.title().to_lowercase() == needle
        {
            base_score + 20_000
        } else if entry.id().to_lowercase().contains(&needle)
            || entry.title().to_lowercase().contains(&needle)
        {
            base_score + 10_000
        } else {
            base_score
        };
        hits.push((entry.clone(), score));
    }
    hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id().cmp(b.0.id())));
    hits.truncate(limit);
    hits
}

trait Searchable {
    fn id(&self) -> &str;
    fn title(&self) -> &str;
    fn searchable(&self) -> String;
}

impl Searchable for PeerEntry {
    fn id(&self) -> &str {
        &self.id
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn searchable(&self) -> String {
        PeerEntry::searchable(self)
    }
}

impl Searchable for ServiceEntry {
    fn id(&self) -> &str {
        &self.id
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn searchable(&self) -> String {
        ServiceEntry::searchable(self)
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn array_field<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
