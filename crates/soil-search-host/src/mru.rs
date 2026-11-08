//! Host-owned usage memory: what a launcher shows before anyone types.
//!
//! Usage is the one launcher input that is not derived. The VFS can be re-indexed
//! and applications re-enumerated at any time, but which rows a person actually
//! picks cannot be recovered from anything else, so it lives in its own file
//! rather than in `soil:index`, which is explicitly rebuildable.
//!
//! The file caches the display row beside the id. That is the point: the first
//! frame after a summon must not wait on an index scan, so answering it costs one
//! file read, not a query.

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{api, kind_name, store};

/// Rows kept on disk. Past this the tail is history nobody would scroll to.
const MAX_ENTRIES: usize = 256;
const FORMAT_VERSION: u32 = 1;
/// The most a remembered row can gain over an unremembered one with the same
/// words in it. Large enough to order a launcher's first frame, small enough that
/// an exact title match still wins.
const MAX_BOOST: u32 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub source: String,
    pub uses: u32,
    pub last_used_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct File {
    version: u32,
    entries: Vec<Usage>,
}

/// A recency-ordered memory of picked rows, optionally backed by a file.
pub struct MruCache {
    path: Option<PathBuf>,
    entries: Vec<Usage>,
}

impl MruCache {
    /// Memory only. The right default for a short-lived host and for tests.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: Vec::new(),
        }
    }

    pub fn open(path: impl Into<PathBuf>) -> Self {
        let mut cache = Self {
            path: Some(path.into()),
            entries: Vec::new(),
        };
        cache.load();
        cache
    }

    fn load(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        // Missing is the normal first run; unreadable is reported by `store` and
        // treated as absent, because a cache that cannot be parsed is history.
        if let Some(file) = store::read::<File>(path, "MRU cache")
            && file.version == FORMAT_VERSION
        {
            self.entries = file.entries;
            self.entries.sort_by(|a, b| {
                b.last_used_ms
                    .cmp(&a.last_used_ms)
                    .then_with(|| a.id.cmp(&b.id))
            });
            self.entries.truncate(MAX_ENTRIES);
        }
    }

    /// Most recent first. The caller renders these; it never re-sorts them.
    pub fn recent(&self, limit: usize) -> &[Usage] {
        &self.entries[..limit.min(self.entries.len())]
    }

    /// Count one use of `usage`, refreshing the display row with what the host saw
    /// this time, and persist. Callers record a launch, not a render.
    pub fn record(&mut self, mut usage: Usage) -> anyhow::Result<()> {
        usage.uses = 1;
        match self.entries.iter_mut().find(|entry| entry.id == usage.id) {
            Some(existing) => {
                usage.uses = existing.uses.saturating_add(1);
                *existing = usage;
            }
            None => self.entries.push(usage),
        }
        self.entries.sort_by(|a, b| {
            b.last_used_ms
                .cmp(&a.last_used_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        self.entries.truncate(MAX_ENTRIES);
        self.save()
    }

    /// How much a remembered row gains over an equally good stranger.
    pub fn boost(&self, id: &str, now_ms: u64) -> u32 {
        let Some(usage) = self.entries.iter().find(|entry| entry.id == id) else {
            return 0;
        };
        let scaled = recency(usage.uses, usage.last_used_ms, now_ms) * 20_000.0;
        (scaled.round() as u32).min(MAX_BOOST)
    }

    fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        store::write(
            path,
            &File {
                version: FORMAT_VERSION,
                entries: self.entries.clone(),
            },
        )
    }
}

/// Recency-weighted frequency, the same curve `spores/search` applies to the
/// frecency it is handed: frequency is logarithmic so the tenth use matters less
/// than the second, and recency saturates at a day and decays to a thousand.
pub fn recency(uses: u32, last_used_ms: u64, now_ms: u64) -> f64 {
    let age_days = (now_ms.saturating_sub(last_used_ms) as f64 / 86_400_000.0).clamp(1.0, 1000.0);
    (1.0 + f64::from(uses)).ln() / (1.0 + age_days)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

pub fn usage_from_hit(hit: &api::Hit, last_used_ms: u64) -> Usage {
    Usage {
        id: hit.id.clone(),
        kind: kind_name(hit.kind).to_string(),
        title: hit.title.clone(),
        subtitle: hit.subtitle.clone(),
        source: hit.source.clone(),
        uses: 1,
        last_used_ms,
    }
}

/// A remembered row as a hit. Rank becomes score so a caller that re-sorts keeps
/// the order the cache handed over.
pub fn hit_from_usage(usage: &Usage, rank: usize) -> api::Hit {
    api::Hit {
        kind: crate::kind_from_name(&usage.kind).unwrap_or(api::Kind::File),
        id: usage.id.clone(),
        title: usage.title.clone(),
        subtitle: usage.subtitle.clone(),
        snippet: None,
        score: 1_000_000u32.saturating_sub(rank as u32),
        source: usage.source.clone(),
        updated_at_ms: None,
        references: Vec::new(),
        icon: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn usage(id: &str, at: u64) -> Usage {
        Usage {
            id: id.into(),
            kind: "application".into(),
            title: id.into(),
            subtitle: None,
            source: "host:applications".into(),
            uses: 1,
            last_used_ms: at,
        }
    }

    #[test]
    fn recording_again_counts_a_use_and_moves_the_row_to_the_front() {
        let mut cache = MruCache::in_memory();
        cache.record(usage("a", 10)).unwrap();
        cache.record(usage("b", 20)).unwrap();
        assert_eq!(cache.recent(1)[0].id, "b");

        cache.record(usage("a", 30)).unwrap();
        assert_eq!(cache.recent(1)[0].id, "a");
        assert_eq!(cache.recent(1)[0].uses, 2);
    }

    #[test]
    fn the_cache_survives_a_restart_and_serves_the_cached_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/mru.json");
        {
            let mut cache = MruCache::open(path.clone());
            cache.record(usage("com.apple.Safari", 1_000)).unwrap();
        }
        let cache = MruCache::open(path);
        let recent = cache.recent(8);
        assert_eq!(recent.len(), 1);
        // The first frame reads this row without touching a source.
        assert_eq!(recent[0].title, "com.apple.Safari");
        assert_eq!(recent[0].kind, "application");
    }

    #[test]
    fn a_corrupt_cache_is_history_nobody_has_to_fix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mru.json");
        fs::write(&path, b"{ not json").unwrap();
        let cache = MruCache::open(path);
        assert!(cache.recent(8).is_empty());
    }

    #[test]
    fn a_remembered_row_outranks_a_stranger_and_old_use_fades() {
        let now = 1_700_000_000_000u64;
        let mut cache = MruCache::in_memory();
        cache.record(usage("today", now - 1_000)).unwrap();
        assert!(cache.boost("today", now) > 0);
        assert_eq!(cache.boost("never-used", now), 0);

        let fresh = cache.boost("today", now);
        let later = cache.boost("today", now + 30 * 86_400_000);
        assert!(later < fresh, "old use should fade: {later} !< {fresh}");
        assert!(fresh <= MAX_BOOST);
    }

    #[test]
    fn frequent_rows_outrank_rare_ones_of_the_same_age() {
        let now = 1_700_000_000_000u64;
        let mut cache = MruCache::in_memory();
        let mut many = usage("many", now - 1_000);
        many.uses = 40;
        cache.entries.push(many);
        cache.entries.push(usage("once", now - 1_000));
        assert!(cache.boost("many", now) > cache.boost("once", now));
    }
}
