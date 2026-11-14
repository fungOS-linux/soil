//! Host-side Spore installation, update application, and rollback.
//!
//! The store presenter and planner stay read-only. This crate is the host
//! authority that writes packages, keeps the package cache, and swaps an installed
//! directory only after the tar, lock and optional publisher signature verify.
//!
//! Layout under the install root:
//!
//! ```text
//! <root>/<spore-name>/          unpacked current package, named the way a
//!                                catalog scan names it; the id lives in .install/
//! <root>/.packages/<digest>.spore   immutable tars, including the previous one
//! <root>/.install/<spore-id>.json   current/previous digest and publisher
//! <root>/.staging/                  interrupted swaps only
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const META_DIR: &str = ".install";
pub const PACKAGE_DIR: &str = ".packages";
pub const STAGING_DIR: &str = ".staging";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Spore(#[from] soil_spore::Error),
    #[error("store: {0}")]
    Store(String),
    #[error("io: {0}")]
    Io(String),
    #[error("invalid install: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
}

impl From<soil_store::Error> for Error {
    fn from(error: soil_store::Error) -> Self {
        Self::Store(error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledState {
    pub id: String,
    /// The directory this tree lives in. Trees are named by package, not by id,
    /// because that is how every existing install and every catalog scan names
    /// them; older state files predate the field and fall back to the id.
    #[serde(default)]
    pub name: Option<String>,
    pub version: String,
    pub digest: String,
    pub previous_digest: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    pub installed_at: u64,
}

#[derive(Clone, Copy, Default)]
pub struct InstallOptions<'a> {
    pub keyring: Option<&'a Path>,
    pub key: Option<&'a str>,
    pub publisher: Option<&'a str>,
    /// A delegated publisher public key. When present, it replaces the local
    /// keyring for this package's sidecar signature.
    pub public_key: Option<&'a [u8]>,
}

/// Install a verified package, replacing the current directory only after the
/// new tree is unpacked. The previous tar stays in `.packages` for rollback.
pub fn install(
    package: &Path,
    root: &Path,
    options: InstallOptions<'_>,
) -> Result<InstalledState, Error> {
    let bytes =
        fs::read(package).map_err(|error| Error::Io(format!("{}: {error}", package.display())))?;
    let (manifest, lock) = soil_spore::verify(&bytes)?;
    if let Some(public_key) = options.public_key {
        let signature = signature_path(package);
        let signature_bytes = fs::read(&signature)
            .map_err(|error| Error::Io(format!("{}: {error}", signature.display())))?;
        soil_spore::verify_detached(public_key, &bytes, &signature_bytes)?;
    } else if options.keyring.is_some() || options.key.is_some() {
        let signature = signature_path(package);
        soil_spore::verify_signature(package, &signature, options.keyring, options.key)?;
    }

    let id = manifest.spore.id.clone();
    validate_id(&id)?;
    let existing = read_state(root, &id).ok();
    if existing
        .as_ref()
        .is_some_and(|state| state.digest == lock.digest)
    {
        return Ok(existing.expect("checked above"));
    }

    ensure_layout(root)?;
    let staging = root
        .join(STAGING_DIR)
        .join(format!("{id}-{}-{}", std::process::id(), nanos()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|error| Error::Io(format!("{}: {error}", staging.display())))?;
    }
    soil_spore::unpack(&bytes, &staging)?;

    let cached = package_path(root, &lock.digest);
    if !cached.is_file() {
        let temporary = cached.with_extension("spore.tmp");
        fs::write(&temporary, &bytes)
            .map_err(|error| Error::Io(format!("{}: {error}", temporary.display())))?;
        fs::rename(&temporary, &cached)
            .map_err(|error| Error::Io(format!("{}: {error}", cached.display())))?;
    }

    let name = manifest.spore.name.clone();
    validate_id(&name)?;
    let legacy = root.join(&id);
    if name != id && legacy.exists() && !root.join(&name).exists() {
        fs::rename(&legacy, root.join(&name))
            .map_err(|error| Error::Io(format!("{}: {error}", legacy.display())))?;
    }
    let current = root.join(&name);
    let backup =
        root.join(STAGING_DIR)
            .join(format!("{id}-old-{}-{}", std::process::id(), nanos()));
    if current.exists() {
        fs::rename(&current, &backup)
            .map_err(|error| Error::Io(format!("{}: {error}", current.display())))?;
    }
    if let Err(error) = fs::rename(&staging, &current) {
        if backup.exists() {
            let _ = fs::rename(&backup, &current);
        }
        return Err(Error::Io(format!("{}: {error}", current.display())));
    }
    let _ = fs::remove_dir_all(&backup);

    let state = InstalledState {
        id,
        name: Some(name),
        version: manifest.spore.version,
        digest: lock.digest,
        previous_digest: existing.as_ref().map(|state| state.digest.clone()),
        publisher: options
            .publisher
            .map(str::to_owned)
            .or_else(|| existing.and_then(|state| state.publisher)),
        installed_at: now(),
    };
    write_state(root, &state)?;
    Ok(state)
}

/// Roll back to the one previous digest. The tar must still be in `.packages`.
pub fn rollback(id: &str, root: &Path) -> Result<InstalledState, Error> {
    validate_id(id)?;
    let state = read_state(root, id)?;
    let previous = state
        .previous_digest
        .clone()
        .ok_or_else(|| Error::NotFound(format!("{id} has no previous digest")))?;
    let package = package_path(root, &previous);
    let bytes =
        fs::read(&package).map_err(|error| Error::Io(format!("{}: {error}", package.display())))?;
    let (manifest, lock) = soil_spore::verify(&bytes)?;
    if manifest.spore.id != id {
        return Err(Error::Invalid(format!(
            "{} contains {}, not {id}",
            package.display(),
            manifest.spore.id
        )));
    }
    if lock.digest != previous {
        return Err(Error::Invalid(
            "cached tar does not match the previous digest".into(),
        ));
    }

    let staging =
        root.join(STAGING_DIR)
            .join(format!("{id}-rollback-{}-{}", std::process::id(), nanos()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|error| Error::Io(format!("{}: {error}", staging.display())))?;
    }
    soil_spore::unpack(&bytes, &staging)?;
    let current = root.join(state.name.as_deref().unwrap_or(id));
    let backup = root.join(STAGING_DIR).join(format!(
        "{id}-rollback-old-{}-{}",
        std::process::id(),
        nanos()
    ));
    if current.exists() {
        fs::rename(&current, &backup)
            .map_err(|error| Error::Io(format!("{}: {error}", current.display())))?;
    }
    if let Err(error) = fs::rename(&staging, &current) {
        if backup.exists() {
            let _ = fs::rename(&backup, &current);
        }
        return Err(Error::Io(format!("{}: {error}", current.display())));
    }
    let _ = fs::remove_dir_all(&backup);

    let next = InstalledState {
        id: id.to_owned(),
        name: state.name.clone(),
        version: manifest.spore.version,
        digest: lock.digest,
        previous_digest: Some(state.digest),
        publisher: state.publisher,
        installed_at: now(),
    };
    write_state(root, &next)?;
    Ok(next)
}

pub fn status(root: &Path) -> Result<Vec<InstalledState>, Error> {
    let dir = root.join(META_DIR);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut states = Vec::new();
    for entry in
        fs::read_dir(&dir).map_err(|error| Error::Io(format!("{}: {error}", dir.display())))?
    {
        let entry = entry.map_err(|error| Error::Io(error.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let bytes =
            fs::read(&path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        let state: InstalledState = serde_json::from_slice(&bytes)
            .map_err(|error| Error::Invalid(format!("{}: {error}", path.display())))?;
        states.push(state);
    }
    states.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(states)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplyOptions<'a> {
    pub index: &'a Path,
    pub packages: &'a Path,
    /// Armored store-operator public key. Every delegation must verify against
    /// it, and every applied package must match the delegated publisher key.
    pub operator_key: &'a Path,
    pub write: bool,
    pub allow_unsigned_dev: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApplyOutcome {
    pub id: String,
    pub from: String,
    pub to: String,
    pub digest: String,
    pub class: String,
    pub action: String,
}

/// Apply the updates the planner classifies as `live` or `restart`.
/// `consent`, `system` and `hold` are reported and never applied.
pub fn apply(root: &Path, options: ApplyOptions<'_>) -> Result<Vec<ApplyOutcome>, Error> {
    let operator_key = fs::read(options.operator_key)
        .map_err(|error| Error::Io(format!("{}: {error}", options.operator_key.display())))?;
    let index = soil_store::Index::read(options.index)?;
    let verified = if options.allow_unsigned_dev {
        None
    } else {
        Some(index.verify(&operator_key)?)
    };
    let catalog = index.catalog();
    let installed = scan_installed(root)?;
    let plan = soil_store::plan(&installed, &catalog);
    let mut outcomes = Vec::new();
    for update in plan.updates {
        let class = update.class.as_str().to_owned();
        if !matches!(
            update.class,
            soil_store::Class::Live | soil_store::Class::Restart
        ) {
            let action = match update.class {
                soil_store::Class::Consent => "staged: needs consent",
                soil_store::Class::System => "staged: needs reboot",
                soil_store::Class::Hold => "held by policy",
                soil_store::Class::Live | soil_store::Class::Restart => unreachable!(),
            };
            outcomes.push(ApplyOutcome {
                id: update.spore_id,
                from: update.from,
                to: update.to,
                digest: update.digest,
                class,
                action: action.into(),
            });
            continue;
        }
        let package = options.packages.join(format!("{}.spore", update.digest));
        if !package.is_file() {
            outcomes.push(ApplyOutcome {
                id: update.spore_id,
                from: update.from,
                to: update.to,
                digest: update.digest,
                class,
                action: "missing package tar".into(),
            });
            continue;
        }
        if !options.write {
            outcomes.push(ApplyOutcome {
                id: update.spore_id,
                from: update.from,
                to: update.to,
                digest: update.digest,
                class,
                action: "would install".into(),
            });
            continue;
        }
        let entry = catalog.get(&update.spore_id);
        if verified.is_some() && entry.is_none() {
            return Err(Error::Invalid(format!(
                "{}: update has no catalog entry",
                update.spore_id
            )));
        }
        let public_key = match (&verified, entry) {
            (Some(verified), Some(entry)) => Some(
                verified
                    .for_entry(entry)
                    .ok_or_else(|| {
                        Error::Invalid(format!(
                            "{}: update entry is not delegated",
                            update.spore_id
                        ))
                    })?
                    .public_key
                    .as_bytes(),
            ),
            _ => None,
        };
        let state = install(
            &package,
            root,
            InstallOptions {
                keyring: None,
                key: None,
                publisher: entry.and_then(|entry| entry.publisher.as_deref()),
                public_key,
            },
        )?;
        outcomes.push(ApplyOutcome {
            id: state.id,
            from: update.from,
            to: state.version,
            digest: state.digest,
            class,
            action: "installed".into(),
        });
    }
    Ok(outcomes)
}

fn scan_installed(root: &Path) -> Result<Vec<soil_store::Installed>, Error> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut installed = Vec::new();
    for entry in
        fs::read_dir(root).map_err(|error| Error::Io(format!("{}: {error}", root.display())))?
    {
        let entry = entry.map_err(|error| Error::Io(error.to_string()))?;
        let path = entry.path();
        if !path.is_dir() || !path.join("spore.toml").is_file() {
            continue;
        }
        installed.push(soil_store::Installed::read(&path, None, None)?);
    }
    Ok(installed)
}

fn ensure_layout(root: &Path) -> Result<(), Error> {
    for dir in [META_DIR, PACKAGE_DIR, STAGING_DIR] {
        let path = root.join(dir);
        fs::create_dir_all(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    }
    Ok(())
}

fn read_state(root: &Path, id: &str) -> Result<InstalledState, Error> {
    let path = state_path(root, id);
    let bytes =
        fs::read(&path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    let state: InstalledState = serde_json::from_slice(&bytes)
        .map_err(|error| Error::Invalid(format!("{}: {error}", path.display())))?;
    if state.id != id {
        return Err(Error::Invalid(format!("{id} state names {}", state.id)));
    }
    Ok(state)
}

fn write_state(root: &Path, state: &InstalledState) -> Result<(), Error> {
    ensure_layout(root)?;
    let path = state_path(root, &state.id);
    let temporary = path.with_extension("json.tmp");
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|error| Error::Invalid(error.to_string()))?;
    fs::write(&temporary, bytes)
        .map_err(|error| Error::Io(format!("{}: {error}", temporary.display())))?;
    fs::rename(&temporary, &path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))
}

fn state_path(root: &Path, id: &str) -> PathBuf {
    root.join(META_DIR).join(format!("{id}.json"))
}

fn package_path(root: &Path, digest: &str) -> PathBuf {
    root.join(PACKAGE_DIR).join(format!("{digest}.spore"))
}

fn signature_path(package: &Path) -> PathBuf {
    let mut path = package.to_path_buf();
    path.set_extension(format!(
        "{}.asc",
        package
            .extension()
            .map_or("spore", |extension| extension.to_str().unwrap_or("spore"))
    ));
    path
}

fn validate_id(id: &str) -> Result<(), Error> {
    if id.is_empty()
        || id.len() > 128
        || id.contains("..")
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Error::Invalid(format!("unsafe Spore id {id:?}")));
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CID: &str = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";

    fn fixture(version: &str, body: &str) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ui")).unwrap();
        std::fs::write(
            dir.path().join("spore.toml"),
            format!(
                r#"schema = 1
[spore]
id = "dev.fungos.install-test"
name = "install-test"
version = "{version}"
license = "MIT"
summary = "install test"
[engine]
soil = ">=0.1"
target = "soil"
[ui]
entry = "ui/main.lua"
[launch]
default = "canvas"
surfaces = ["canvas"]
[grants]
"soil:canvas" = "present the test view"
"#
            ),
        )
        .unwrap();
        std::fs::write(dir.path().join("ui/main.lua"), body).unwrap();
        soil_spore::pack(dir.path()).unwrap()
    }

    fn write_package(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn install_and_rollback_swap_the_installed_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("spores");
        let one = temp.path().join("one.spore");
        let two = temp.path().join("two.spore");
        write_package(&one, &fixture("0.1.0", "return 'one'"));
        write_package(&two, &fixture("0.2.0", "return 'two'"));

        let first = install(&one, &root, InstallOptions::default()).unwrap();
        assert_eq!(first.version, "0.1.0");
        assert_eq!(first.previous_digest, None);
        assert_eq!(
            std::fs::read_to_string(root.join(&first.id).join("ui/main.lua")).unwrap(),
            "return 'one'"
        );

        let second = install(&two, &root, InstallOptions::default()).unwrap();
        assert_eq!(second.version, "0.2.0");
        assert_eq!(
            second.previous_digest.as_deref(),
            Some(first.digest.as_str())
        );
        assert_eq!(
            std::fs::read_to_string(root.join(&second.id).join("ui/main.lua")).unwrap(),
            "return 'two'"
        );

        let rolled = rollback(&second.id, &root).unwrap();
        assert_eq!(rolled.version, "0.1.0");
        assert_eq!(rolled.digest, first.digest);
        assert_eq!(
            rolled.previous_digest.as_deref(),
            Some(second.digest.as_str())
        );
        assert_eq!(
            std::fs::read_to_string(root.join(&rolled.id).join("ui/main.lua")).unwrap(),
            "return 'one'"
        );
        assert_eq!(status(&root).unwrap().len(), 1);
    }

    #[test]
    fn apply_installs_only_live_or_restart_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("spores");
        let packages = temp.path().join("packages");
        std::fs::create_dir_all(&packages).unwrap();

        let one = temp.path().join("one.spore");
        write_package(&one, &fixture("0.1.0", "return 'one'"));
        install(&one, &root, InstallOptions::default()).unwrap();

        let two_bytes = fixture("0.2.0", "return 'two'");
        let (_, two_lock) = soil_spore::verify(&two_bytes).unwrap();
        write_package(
            &packages.join(format!("{}.spore", two_lock.digest)),
            &two_bytes,
        );

        let mut entry = soil_store::Entry {
            seq: 0,
            kind: soil_store::EntryKind::Update,
            spore_id: "dev.fungos.install-test".into(),
            version: "0.2.0".into(),
            digest: two_lock.digest.clone(),
            package: CID.into(),
            listing: None,
            module: None,
            ui: Some("changed".into()),
            grants: vec!["soil:canvas".into()],
            system: None,
            publisher: Some("mycelium:test".into()),
            publisher_key: None,
            signature: String::new(),
        };
        entry.seq = 1;
        let index = soil_store::Index::seal("test", vec![entry]);
        let index_path = temp.path().join("index.json");
        std::fs::write(&index_path, serde_json::to_vec(&index).unwrap()).unwrap();
        let operator_key = temp.path().join("operator.asc");
        std::fs::write(&operator_key, b"unsigned dev").unwrap();

        let refused = apply(
            &root,
            ApplyOptions {
                index: &index_path,
                packages: &packages,
                operator_key: &operator_key,
                write: true,
                allow_unsigned_dev: false,
            },
        );
        assert!(refused.is_err(), "unsigned index must not install");

        let outcomes = apply(
            &root,
            ApplyOptions {
                index: &index_path,
                packages: &packages,
                operator_key: &operator_key,
                write: true,
                allow_unsigned_dev: true,
            },
        )
        .unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].action, "installed");
        assert_eq!(status(&root).unwrap()[0].version, "0.2.0");
    }

    #[test]
    fn install_is_idempotent_for_the_same_digest() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("spores");
        let package = temp.path().join("one.spore");
        write_package(&package, &fixture("0.1.0", "return 'one'"));
        let first = install(&package, &root, InstallOptions::default()).unwrap();
        let second = install(&package, &root, InstallOptions::default()).unwrap();
        assert_eq!(first, second);
    }
}

/// What `adopt` found, and what it recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adopted {
    pub id: String,
    pub version: String,
    pub digest: String,
    pub action: String,
}

/// Record provenance for trees that were put here by hand.
///
/// Until a tree has `.install/<id>.json`, soil cannot tell current from drifted
/// from adopted, so every later decision - sync, update, rollback - is guesswork.
/// Adoption does not trust the tree; it hashes it, which is exactly the claim
/// provenance makes. The publisher is left empty because nobody signed it here.
pub fn adopt(root: &Path, overwrite: bool) -> Result<Vec<Adopted>, Error> {
    ensure_layout(root)?;
    let mut adopted = Vec::new();
    let entries =
        fs::read_dir(root).map_err(|error| Error::Io(format!("{}: {error}", root.display())))?;
    for entry in entries {
        let path = entry.map_err(|error| Error::Io(error.to_string()))?.path();
        if !path.is_dir() || !path.join("spore.toml").is_file() {
            continue;
        }
        let fallback = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?")
            .to_string();
        // A tree that does not check is the interesting case, not the fatal
        // one: adoption is what makes it visible instead of invisible.
        let manifest = match soil_spore::Manifest::load(&path) {
            Ok(manifest) => manifest,
            Err(error) => {
                adopted.push(Adopted {
                    id: fallback,
                    version: String::new(),
                    digest: String::new(),
                    action: format!("skipped: {error}"),
                });
                continue;
            }
        };
        let id = manifest.spore.id.clone();
        validate_id(&id)?;
        if !overwrite && state_path(root, &id).is_file() {
            continue;
        }
        let lock = match soil_spore::check(&path) {
            Ok(lock) => lock,
            Err(error) => {
                adopted.push(Adopted {
                    id,
                    version: manifest.spore.version.clone(),
                    digest: String::new(),
                    action: format!("skipped: {error}"),
                });
                continue;
            }
        };
        let state = InstalledState {
            id: id.clone(),
            name: Some(manifest.spore.name.clone()),
            version: manifest.spore.version.clone(),
            digest: lock.digest.clone(),
            previous_digest: None,
            publisher: None,
            installed_at: now(),
        };
        write_state(root, &state)?;
        adopted.push(Adopted {
            id,
            version: manifest.spore.version.clone(),
            digest: lock.digest,
            action: if overwrite {
                "adopted (overwrote provenance)".into()
            } else {
                "adopted".into()
            },
        });
    }
    Ok(adopted)
}

/// What `sync` would do, or did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOutcome {
    pub id: String,
    /// The role that put this package in the seed.
    pub role: String,
    pub from: Option<String>,
    pub to: String,
    pub digest: String,
    pub action: String,
}

pub struct SyncOptions<'a> {
    /// A directory holding a signed index and its operator key: `index.json`
    /// and `operator.asc`.
    pub seed: &'a Path,
    /// Where the tars live. Defaults to `<seed>/packages`.
    pub packages: Option<&'a Path>,
    pub root: &'a Path,
    pub write: bool,
}

/// Install the role-bearing part of a seed index.
///
/// A role is the contract: `store` and `launcher` are things a host with soil is
/// supposed to have, and the seed names which package fills each one today. Only
/// entries that declare a `[system] role` are considered, so shipping an index
/// does not install everything it lists.
pub fn sync(root: &Path, options: SyncOptions<'_>) -> Result<Vec<SyncOutcome>, Error> {
    let index_path = options.seed.join("index.json");
    let operator_key = fs::read(options.seed.join("operator.asc"))
        .map_err(|error| Error::Io(format!("{}: {error}", operator_key_path(options.seed).display())))?;
    let index = soil_store::Index::read(&index_path)?;
    let verified = index.verify(&operator_key)?;
    let packages = match options.packages {
        Some(path) => path.to_path_buf(),
        None => options.seed.join("packages"),
    };
    let installed = scan_installed(root)?;
    let mut outcomes = Vec::new();
    for entry in &index.entries {
        let Some(role) = entry.system.as_ref() else {
            continue;
        };
        let current = installed
            .iter()
            .find(|app| app.manifest.spore.id == entry.spore_id)
            .map(|app| app.digest.clone());
        let action = match &current {
            Some(digest) if digest == &entry.digest => "current",
            Some(_) => "would update",
            None => "would install",
        };
        if !options.write || action == "current" {
            outcomes.push(SyncOutcome {
                id: entry.spore_id.clone(),
                role: role.clone(),
                from: current,
                to: entry.version.clone(),
                digest: entry.digest.clone(),
                action: action.into(),
            });
            continue;
        }
        let package = packages.join(format!("{}.spore", entry.digest));
        if !package.is_file() {
            outcomes.push(SyncOutcome {
                id: entry.spore_id.clone(),
                role: role.clone(),
                from: current,
                to: entry.version.clone(),
                digest: entry.digest.clone(),
                action: "missing package tar".into(),
            });
            continue;
        }
        let delegation = verified.for_entry(entry).ok_or_else(|| {
            Error::Invalid(format!("{}: seed entry is not delegated", entry.spore_id))
        })?;
        install(
            &package,
            root,
            InstallOptions {
                public_key: Some(delegation.public_key.as_bytes()),
                publisher: entry.publisher.as_deref(),
                ..Default::default()
            },
        )?;
        outcomes.push(SyncOutcome {
            id: entry.spore_id.clone(),
            role: role.clone(),
            from: current,
            to: entry.version.clone(),
            digest: entry.digest.clone(),
            action: if action == "would update" {
                "updated".into()
            } else {
                "installed".into()
            },
        });
    }
    Ok(outcomes)
}

fn operator_key_path(seed: &Path) -> PathBuf {
    seed.join("operator.asc")
}

/// Resolve a package by its content id instead of a path.
///
/// The index is what knows which digest and which publisher key a CID belongs
/// to, so a CID installs the same way a tar does once it is found: the cache
/// first, then the store origin. A peer fetch is the same lookup with a
/// different transport.
pub fn install_cid(
    cid: &str,
    index: &Path,
    packages: &Path,
    origin: Option<&str>,
    root: &Path,
) -> Result<InstalledState, Error> {
    let index = soil_store::Index::read(index)?;
    let entry = index
        .entries
        .iter()
        .find(|entry| entry.package == cid)
        .ok_or_else(|| Error::NotFound(format!("no index entry carries {cid}")))?;
    let package = packages.join(format!("{}.spore", entry.digest));
    if !package.is_file() {
        let origin = origin.ok_or_else(|| {
            Error::NotFound(format!(
                "{} is not cached and no --origin was given",
                entry.digest
            ))
        })?;
        let url = format!("{}/package/{}", origin.trim_end_matches('/'), entry.digest);
        let response = ureq::get(&url)
            .call()
            .map_err(|error| Error::Store(format!("{url}: {error}")))?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|error| Error::Io(format!("{url}: {error}")))?;
        fs::write(&package, &bytes)
            .map_err(|error| Error::Io(format!("{}: {error}", package.display())))?;
    }
    let bytes =
        fs::read(&package).map_err(|error| Error::Io(format!("{}: {error}", package.display())))?;
    let found = soil_store::cid::raw(&bytes);
    if found != cid {
        return Err(Error::Invalid(format!(
            "{} hashes to {found}, not {cid}",
            package.display()
        )));
    }
    install(
        &package,
        root,
        InstallOptions {
            publisher: entry.publisher.as_deref(),
            ..Default::default()
        },
    )
}
