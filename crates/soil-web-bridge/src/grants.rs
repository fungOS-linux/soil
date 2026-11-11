//! A Spore's `[grants]` table, as a set a request can be checked against.
//!
//! Grant keys carry an optional sub-capability after a dot:
//!
//! ```toml
//! [grants]
//! "soil:records.read" = "read the worklog collection"
//! "soil:canvas" = "present the surface"
//! ```
//!
//! `soil:records.read` allows `scan`; `soil:records` allows the whole package.
//! The rule is one line — a dotted key names a part, a bare key names the whole
//! — and it is the same rule the wasmtime host applies when it links imports.
//! Reading the manifest here instead of inventing a bridge-side ACL is what
//! keeps `denied` meaning the same thing in a browser and on a desktop.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use soil_spore::Manifest;

/// The grants one Spore package declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantSet {
    /// Package -> `None` for a whole-package grant, `Some(parts)` for named
    /// sub-capabilities. `soil:records` and `soil:records.read` can both appear;
    /// the whole wins.
    scopes: BTreeMap<String, Option<BTreeSet<String>>>,
    /// The keys exactly as written, so `ready` can show a person what the
    /// package asked for rather than a normalised form they cannot grep for.
    declared: Vec<String>,
}

impl GrantSet {
    /// Load `spore.toml` and read its grants.
    ///
    /// `Manifest::load` parses and validates the manifest; it does not check the
    /// module's imports (that is `soil-spore check`, which needs a built
    /// module). The bridge only needs the declared intent, and a package that
    /// lies about it is caught by the packager, not here.
    pub fn load(spore_dir: &Path) -> anyhow::Result<Self> {
        let manifest = Manifest::load(spore_dir)
            .with_context(|| format!("read {}", spore_dir.join("spore.toml").display()))?;
        Ok(Self::from_manifest(&manifest))
    }

    pub fn from_manifest(manifest: &Manifest) -> Self {
        let mut set = Self::default();
        for key in manifest.grants.keys() {
            set.declared.push(key.clone());
            set.insert(key);
        }
        set
    }

    fn insert(&mut self, key: &str) {
        match key.split_once('.') {
            Some((package, part)) => {
                let parts = self
                    .scopes
                    .entry(package.to_owned())
                    .or_insert_with(|| Some(BTreeSet::new()));
                if let Some(parts) = parts {
                    parts.insert(part.to_owned());
                }
            }
            None => {
                self.scopes.insert(key.to_owned(), None);
            }
        }
    }

    /// Is `package` allowed to use `capability`?
    ///
    /// A bare package grant allows every capability in it. A dotted grant
    /// allows exactly the parts it names, and nothing else.
    pub fn allows(&self, package: &str, capability: &str) -> bool {
        match self.scopes.get(package) {
            None => false,
            Some(None) => true,
            Some(Some(parts)) => parts.contains(capability),
        }
    }

    /// The grant keys as the manifest wrote them, sorted for a stable `ready`.
    pub fn declared(&self) -> Vec<String> {
        let mut declared = self.declared.clone();
        declared.sort();
        declared.dedup();
        declared
    }

    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(keys: &[&str]) -> GrantSet {
        let mut set = GrantSet::default();
        for key in keys {
            set.insert(key);
        }
        set
    }

    #[test]
    fn a_bare_package_grant_allows_every_part() {
        let grants = set(&["soil:records"]);
        assert!(grants.allows("soil:records", "read"));
        assert!(grants.allows("soil:records", "write"));
        assert!(!grants.allows("soil:canvas", "present"));
    }

    #[test]
    fn a_dotted_grant_allows_only_what_it_names() {
        let grants = set(&["soil:records.read"]);
        assert!(grants.allows("soil:records", "read"));
        assert!(!grants.allows("soil:records", "write"));
    }

    #[test]
    fn the_whole_package_wins_over_a_part() {
        let grants = set(&["soil:records.read", "soil:records"]);
        assert!(grants.allows("soil:records", "write"));
    }

    #[test]
    fn an_unknown_package_is_never_allowed() {
        assert!(!set(&[]).allows("soil:records", "read"));
    }
}
