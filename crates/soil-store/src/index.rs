//! The catalog index: a checkpoint and an append-only entry log.
//!
//! A store does not publish a document that is rewritten. It appends signed
//! entries and publishes a checkpoint saying how far the log goes, so "what
//! changed since I last looked" is a range (`after=<seq>`) and a mirror is just
//! the same bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Operator-signed delegations of publisher keys. They are not part of the
    /// entry log's Merkle root; their own signatures are the authority.
    #[serde(default)]
    pub delegations: Vec<Delegation>,
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
    /// The PGP fingerprint the publisher signed this entry with.
    #[serde(default)]
    pub publisher_key: Option<String>,
    /// Detached armored PGP signature over [`Entry::payload`].
    #[serde(default)]
    pub signature: String,
}

/// A store operator's statement that a publisher key may speak for a
/// principal. The operator signs this once; it does not re-sign every entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Delegation {
    pub publisher: String,
    /// Uppercase hex, 40 or 64 characters.
    pub fingerprint: String,
    /// Armored PGP public key. The operator signature covers its digest.
    pub public_key: String,
    pub scope: String,
    pub issued_at: u64,
    /// `0` means no expiry.
    pub expires_at: u64,
    /// Detached armored PGP signature by the store operator over
    /// [`Delegation::payload`].
    pub signature: String,
}

/// The delegations an [`Index::verify`] call actually validated.
#[derive(Debug, Clone, Default)]
pub struct Verified {
    pub delegations: BTreeMap<(String, String), Delegation>,
}

impl Verified {
    pub fn for_entry(&self, entry: &Entry) -> Option<&Delegation> {
        let publisher = entry.publisher.as_ref()?;
        let fingerprint = entry.publisher_key.as_ref()?.to_ascii_uppercase();
        self.delegations.get(&(publisher.clone(), fingerprint))
    }
}

impl Delegation {
    /// Canonical bytes the store operator signs. The public key is covered by
    /// its digest, so changing either the key or its fingerprint invalidates
    /// the signature.
    pub fn payload(&self) -> String {
        format!(
            "store-delegation 1\npublisher {}\nfingerprint {}\nscope {}\nissued-at {}\nexpires-at {}\nkey-sha256 {}\n",
            self.publisher,
            self.fingerprint,
            self.scope,
            self.issued_at,
            self.expires_at,
            crate::hex(&Sha256::digest(self.public_key.as_bytes())),
        )
    }
}

impl Entry {
    /// Canonical bytes the publisher signs. The signature field is excluded.
    pub fn payload(&self) -> String {
        let mut grants = self.grants.clone();
        grants.sort();
        format!(
            "store-entry 1\nseq {}\nkind {}\nspore {}\nversion {}\ndigest {}\npackage {}\nlisting {}\nmodule {}\nui {}\ngrants {}\nsystem {}\npublisher {}\npublisher-key {}\n",
            self.seq,
            self.kind.as_str(),
            self.spore_id,
            self.version,
            self.digest,
            self.package,
            self.listing.as_deref().unwrap_or("-"),
            self.module.as_deref().unwrap_or("-"),
            self.ui.as_deref().unwrap_or("-"),
            grants.join(","),
            self.system.as_deref().unwrap_or("-"),
            self.publisher.as_deref().unwrap_or("-"),
            self.publisher_key.as_deref().unwrap_or("-"),
        )
    }
}

impl Checkpoint {
    /// Canonical bytes an operator may countersign. The checkpoint is a Merkle
    /// root for truncation/fork detection; the signature is optional.
    pub fn payload(&self, source: &str) -> String {
        format!(
            "store-checkpoint 1\nsource {source}\nseq {}\nroot {}\n",
            self.seq, self.root
        )
    }
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
        let mut delegated = BTreeSet::new();
        for delegation in &self.delegations {
            if delegation.publisher.trim().is_empty() {
                return Err(Error::Invalid("delegation has no publisher".into()));
            }
            if !fingerprint(&delegation.fingerprint) {
                return Err(Error::Invalid(format!(
                    "{}: delegation fingerprint is not 40 or 64 hex",
                    delegation.publisher
                )));
            }
            if delegation.public_key.trim().is_empty() {
                return Err(Error::Invalid(format!(
                    "{}: delegation has no public key",
                    delegation.publisher
                )));
            }
            if delegation.scope.trim().is_empty() {
                return Err(Error::Invalid(format!(
                    "{}: delegation has no scope",
                    delegation.publisher
                )));
            }
            if delegation.expires_at != 0 && delegation.expires_at <= delegation.issued_at {
                return Err(Error::Invalid(format!(
                    "{}: delegation expiry is not after issue",
                    delegation.publisher
                )));
            }
            if !delegated.insert((
                delegation.publisher.clone(),
                delegation.fingerprint.to_ascii_uppercase(),
            )) {
                return Err(Error::Invalid(format!(
                    "{}: duplicate delegation for {}",
                    delegation.publisher, delegation.fingerprint
                )));
            }
        }
        for entry in &self.entries {
            if let Some(key) = &entry.publisher_key
                && !fingerprint(key)
            {
                return Err(Error::Invalid(format!(
                    "{}: publisher key is not 40 or 64 hex",
                    entry.spore_id
                )));
            }
        }
        Ok(())
    }

    /// Verify the operator-to-publisher-to-entry trust chain.
    ///
    /// This is deliberately stronger than [`Self::validate`], which is only
    /// structural. A caller that installs must use this result, not the parsed
    /// index. Each entry must name a delegated publisher key and carry a
    /// signature over [`Entry::payload`]; each delegation must be signed by the
    /// operator key.
    pub fn verify(&self, operator_key: &[u8]) -> Result<Verified> {
        self.validate()?;
        if Self::root(&self.entries) != self.checkpoint.root {
            return Err(Error::Invalid(
                "checkpoint root does not match the entries".into(),
            ));
        }
        if !self.checkpoint.signature.trim().is_empty() {
            soil_spore::verify_detached(
                operator_key,
                self.checkpoint.payload(&self.source).as_bytes(),
                self.checkpoint.signature.as_bytes(),
            )
            .map_err(|error| Error::Invalid(format!("checkpoint signature: {error}")))?;
        }

        let now = now_unix_s();
        let mut verified = Verified::default();
        for delegation in &self.delegations {
            soil_spore::verify_detached(
                operator_key,
                delegation.payload().as_bytes(),
                delegation.signature.as_bytes(),
            )
            .map_err(|error| {
                Error::Invalid(format!(
                    "{} delegation signature: {error}",
                    delegation.publisher
                ))
            })?;
            let key = (
                delegation.publisher.clone(),
                delegation.fingerprint.to_ascii_uppercase(),
            );
            if delegation.expires_at != 0 && now >= delegation.expires_at {
                return Err(Error::Invalid(format!(
                    "{} delegation is expired",
                    delegation.publisher
                )));
            }
            if !matches!(delegation.scope.as_str(), "spore" | "*") {
                return Err(Error::Invalid(format!(
                    "{} delegation scope {:?} cannot sign Spore entries",
                    delegation.publisher, delegation.scope
                )));
            }
            verified.delegations.insert(key, delegation.clone());
        }

        for entry in &self.entries {
            let Some(publisher) = &entry.publisher else {
                return Err(Error::Invalid(format!(
                    "{}: entry has no publisher",
                    entry.spore_id
                )));
            };
            let Some(fingerprint) = &entry.publisher_key else {
                return Err(Error::Invalid(format!(
                    "{}: entry names no publisher key",
                    entry.spore_id
                )));
            };
            let delegation = verified
                .delegations
                .get(&(publisher.clone(), fingerprint.to_ascii_uppercase()))
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "{}: publisher {publisher} key {fingerprint} is not delegated",
                        entry.spore_id
                    ))
                })?;
            soil_spore::verify_detached(
                delegation.public_key.as_bytes(),
                entry.payload().as_bytes(),
                entry.signature.as_bytes(),
            )
            .map_err(|error| {
                Error::Invalid(format!("{} entry signature: {error}", entry.spore_id))
            })?;
        }
        Ok(verified)
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
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    entry.seq,
                    entry.kind.as_str(),
                    entry.spore_id,
                    entry.version,
                    entry.digest,
                    entry.package,
                    entry.listing.as_deref().unwrap_or("-"),
                    entry.publisher_key.as_deref().unwrap_or("-"),
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
    pub fn seal(source: &str, entries: Vec<Entry>) -> Self {
        Self::seal_with_delegations(source, entries, Vec::new())
    }

    pub fn seal_with_delegations(
        source: &str,
        mut entries: Vec<Entry>,
        delegations: Vec<Delegation>,
    ) -> Self {
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
            delegations,
        }
    }
}

fn fingerprint(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

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

    fn gpg_available() -> bool {
        Command::new("gpg")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn generate_key(home: &Path, uid: &str) -> Option<(String, Vec<u8>)> {
        let generated = Command::new("gpg")
            .arg("--homedir")
            .arg(home)
            .args([
                "--batch",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--quick-generate-key",
                uid,
                "ed25519",
                "sign",
                "0",
            ])
            .status()
            .ok()?;
        if !generated.success() {
            return None;
        }
        let listing = Command::new("gpg")
            .arg("--homedir")
            .arg(home)
            .args(["--batch", "--with-colons", "--list-secret-keys"])
            .output()
            .ok()?;
        let listing = String::from_utf8(listing.stdout).ok()?;
        let fingerprint = listing
            .lines()
            .find(|line| line.starts_with("fpr:"))
            .and_then(|line| line.split(':').nth(9))
            .map(str::to_owned)?;
        let public = Command::new("gpg")
            .arg("--homedir")
            .arg(home)
            .args(["--batch", "--armor", "--export", &fingerprint])
            .output()
            .ok()?;
        Some((fingerprint, public.stdout))
    }

    fn sign(home: &Path, fingerprint: &str, payload: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let payload_path = dir.path().join("payload");
        let signature_path = dir.path().join("signature.asc");
        std::fs::write(&payload_path, payload).unwrap();
        let status = Command::new("gpg")
            .arg("--homedir")
            .arg(home)
            .args([
                "--batch",
                "--yes",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--armor",
                "--detach-sign",
                "--local-user",
                fingerprint,
                "--output",
            ])
            .arg(&signature_path)
            .arg(&payload_path)
            .status()
            .unwrap();
        assert!(status.success(), "sign with {fingerprint}");
        std::fs::read_to_string(signature_path).unwrap()
    }

    #[test]
    fn verify_requires_an_operator_delegation_and_publisher_signature() {
        if !gpg_available() {
            eprintln!("gpg unavailable; skipping trust-chain test");
            return;
        }
        let home = tempfile::Builder::new()
            .prefix("soil-store-gpg-")
            .tempdir_in("/tmp")
            .or_else(|_| tempfile::tempdir())
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let (operator_fp, operator_public) =
            generate_key(home.path(), "operator <operator@example.invalid>").unwrap();
        let (publisher_fp, publisher_public) =
            generate_key(home.path(), "publisher <publisher@example.invalid>").unwrap();

        let mut delegation = Delegation {
            publisher: "mycelium:publisher".into(),
            fingerprint: publisher_fp,
            public_key: String::from_utf8(publisher_public).unwrap(),
            scope: "spore".into(),
            issued_at: 1,
            expires_at: 0,
            signature: String::new(),
        };
        delegation.signature = sign(home.path(), &operator_fp, &delegation.payload());

        let mut entry = Entry {
            seq: 0,
            kind: EntryKind::Add,
            spore_id: "dev.fungos.demo".into(),
            version: "0.1.0".into(),
            digest: DIGEST.into(),
            package: CID.into(),
            listing: None,
            module: None,
            ui: Some("ui".into()),
            grants: vec!["soil:canvas".into()],
            system: None,
            publisher: Some("mycelium:publisher".into()),
            publisher_key: Some(delegation.fingerprint.clone()),
            signature: String::new(),
        };
        let mut index = Index::seal_with_delegations("test", vec![entry.clone()], vec![delegation]);
        entry = index.entries[0].clone();
        index.entries[0].signature = sign(
            home.path(),
            &entry.publisher_key.clone().unwrap(),
            &entry.payload(),
        );

        let verified = index.verify(&operator_public).expect("verified chain");
        assert_eq!(verified.delegations.len(), 1);

        let mut tampered = index.clone();
        tampered.entries[0].version = "0.9.9".into();
        assert!(tampered.verify(&operator_public).is_err());

        let mut undelegated = index.clone();
        undelegated.delegations.clear();
        assert!(undelegated.verify(&operator_public).is_err());
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
