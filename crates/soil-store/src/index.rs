//! The catalog index: a checkpoint and an append-only entry log.
//!
//! A store does not publish a document that is rewritten. It appends signed
//! entries and publishes a checkpoint saying how far the log goes, so "what
//! changed since I last looked" is a range (`after=<seq>`) and a mirror is just
//! the same bytes.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result, digest, reference};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub schema: u32,
    /// The source this catalog belongs to, e.g. `fungos` or `home-lab`.
    pub source: String,
    pub checkpoint: Checkpoint,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    /// The last sequence number this checkpoint covers.
    pub seq: u64,
    /// Root over the entries up to `seq`. A client compares roots to detect a
    /// fork or a truncated log.
    pub root: String,
    /// Signature over `(seq, root, source)` by the source principal.
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Add,
    Update,
    Yank,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Update => "update",
            Self::Yank => "yank",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub seq: u64,
    pub kind: EntryKind,
    pub spore_id: String,
    pub version: String,
    /// Package digest (`spore.lock`), the identity an install is bound to.
    pub digest: String,
    /// `ipfs://` reference to the packed tar.
    pub package: String,
    /// `ipfs://` reference to the rich listing (screenshots, prose).
    #[serde(default)]
    pub listing: Option<String>,
    /// SHA-256 of `module.wasm`, or `None` for a composition.
    #[serde(default)]
    pub module: Option<String>,
    /// Digest over the sorted `ui/**/*.lua` hashes: what a hot reload sees.
    #[serde(default)]
    pub ui: Option<String>,
    #[serde(default)]
    pub grants: Vec<String>,
    /// The declared system role, if any. A change is a reboot-class update.
    #[serde(default)]
    pub system: Option<String>,
    /// The publisher principal, so a store can show who signed the package.
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub signature: String,
}

impl Index {
    pub fn parse(source: &str) -> Result<Self> {
        let index: Self =
            serde_json::from_str(source).map_err(|error| Error::Json(error.to_string()))?;
        index.validate()?;
        Ok(index)
    }

    pub fn read(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        Self::parse(&source)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::Invalid(format!(
                "index schema {} is not {SCHEMA}",
                self.schema
            )));
        }
        if self.source.trim().is_empty() {
            return Err(Error::Invalid("index source must not be empty".into()));
        }
        let mut previous = 0;
        for entry in &self.entries {
            if entry.seq <= previous {
                return Err(Error::Invalid(format!(
                    "entry seq {} does not follow {previous}",
                    entry.seq
                )));
            }
            previous = entry.seq;
            if entry.spore_id.trim().is_empty() {
                return Err(Error::Invalid("entry has no spore_id".into()));
            }
            if !digest(&entry.digest) {
                return Err(Error::Invalid(format!(
                    "{}: digest is not a sha256",
                    entry.spore_id
                )));
            }
            if entry.kind != EntryKind::Yank && !reference(&entry.package) {
                return Err(Error::Invalid(format!(
                    "{}: package is not an ipfs:// CID",
                    entry.spore_id
                )));
            }
            if let Some(listing) = &entry.listing
                && !reference(listing)
            {
                return Err(Error::Invalid(format!(
                    "{}: listing is not an ipfs:// CID",
                    entry.spore_id
                )));
            }
        }
        if !self.entries.is_empty() && self.checkpoint.seq < previous {
            return Err(Error::Invalid(format!(
                "checkpoint covers {} but the log ends at {previous}",
                self.checkpoint.seq
            )));
        }
        if self.checkpoint.root.trim().is_empty() {
            return Err(Error::Invalid("checkpoint has no root".into()));
        }
        Ok(())
    }

    /// The newest entry per Spore, after applying `yank`s.
    ///
    /// Later entries win. A yanked package is removed from the catalog rather
    /// than tombstoned here; the log keeps the history, the catalog answers
    /// "what can be installed now".
    pub fn catalog(&self) -> BTreeMap<String, Entry> {
        let mut catalog = BTreeMap::new();
        for entry in &self.entries {
            match entry.kind {
                EntryKind::Add | EntryKind::Update => {
                    catalog.insert(entry.spore_id.clone(), entry.clone());
                }
                EntryKind::Yank => {
                    catalog.remove(&entry.spore_id);
                }
            }
        }
        catalog
    }

    /// The digest a checkpoint commits to: every entry, in order.
    ///
    /// Only fields the store acts on are folded in. A mirror can add a listing
    /// or re-encode the JSON without moving the root; changing a version, a
    /// digest or a package address is visible.
    pub fn root(entries: &[Entry]) -> String {
        let mut hasher = Sha256::new();
        for entry in entries {
            hasher.update(
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    entry.seq,
                    entry.kind.as_str(),
                    entry.spore_id,
                    entry.version,
                    entry.digest,
                    entry.package,
                    entry.listing.as_deref().unwrap_or("-"),
                )
                .as_bytes(),
            );
        }
        crate::hex(&hasher.finalize())
    }

    /// Seal a log: number the entries and commit to them in a checkpoint.
    ///
    /// This is what a publisher runs after appending. It does not sign; the
    /// signature over `(seq, root, source)` is the source principal's, and the
    /// store keeps keys out of the code that builds the catalog.
    pub fn seal(source: &str, mut entries: Vec<Entry>) -> Self {
        for (position, entry) in entries.iter_mut().enumerate() {
            entry.seq = position as u64 + 1;
        }
        Self {
            schema: SCHEMA,
            source: source.to_owned(),
            checkpoint: Checkpoint {
                seq: entries.len() as u64,
                root: Self::root(&entries),
                signature: String::new(),
            },
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CID: &str = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
    const DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn entry(seq: u64, kind: &str, id: &str, version: &str, digest: &str) -> String {
        format!(
            r#"{{"seq":{seq},"kind":"{kind}","spore_id":"{id}","version":"{version}","digest":"{digest}","package":"{CID}"}}"#
        )
    }

    fn index(entries: &[String], checkpoint_seq: u64) -> String {
        format!(
            r#"{{"schema":1,"source":"test","checkpoint":{{"seq":{checkpoint_seq},"root":"r","signature":"s"}},"entries":[{}]}}"#,
            entries.join(",")
        )
    }

    #[test]
    fn the_newest_entry_wins_and_a_yank_removes_it() {
        let parsed = Index::parse(&index(
            &[
                entry(1, "add", "dev.fungos.a", "0.1.0", DIGEST),
                entry(2, "update", "dev.fungos.a", "0.2.0", DIGEST),
                entry(3, "add", "dev.fungos.b", "0.1.0", DIGEST),
            ],
            3,
        ))
        .expect("a well-formed index parses");
        assert_eq!(parsed.catalog()["dev.fungos.a"].version, "0.2.0");
        assert_eq!(parsed.catalog().len(), 2);

        let yanked = Index::parse(&index(
            &[
                entry(1, "add", "dev.fungos.a", "0.1.0", DIGEST),
                entry(2, "yank", "dev.fungos.a", "0.1.0", DIGEST),
            ],
            2,
        ))
        .expect("a yank is an entry");
        assert!(yanked.catalog().is_empty());
    }

    #[test]
    fn a_sequence_number_must_increase() {
        let source = index(
            &[
                entry(2, "add", "dev.fungos.a", "0.1.0", DIGEST),
                entry(2, "add", "dev.fungos.b", "0.1.0", DIGEST),
            ],
            2,
        );
        assert!(matches!(Index::parse(&source), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_checkpoint_cannot_trail_the_log() {
        let source = index(&[entry(5, "add", "dev.fungos.a", "0.1.0", DIGEST)], 4);
        assert!(matches!(Index::parse(&source), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_store_artifact_must_be_a_cid() {
        let bad = entry(1, "add", "dev.fungos.a", "0.1.0", DIGEST).replace(CID, "package.spore");
        assert!(matches!(
            Index::parse(&index(&[bad], 1)),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn a_digest_is_checked_at_the_edge() {
        let bad = entry(1, "add", "dev.fungos.a", "0.1.0", "abc");
        assert!(matches!(
            Index::parse(&index(&[bad], 1)),
            Err(Error::Invalid(_))
        ));
    }
}
