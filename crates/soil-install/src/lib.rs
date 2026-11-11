//! Host-side Spore installation, update application, and rollback.
//!
//! The store presenter and planner stay read-only. This crate is the host
//! authority that writes packages, keeps the package cache, and swaps an installed
//! directory only after the tar, lock and optional publisher signature verify.
//!
//! Layout under the install root:
//!
//! ```text
//! <root>/<spore-id>/            unpacked current package
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
    if options.keyring.is_some() || options.key.is_some() {
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

    let current = root.join(&id);
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
    let current = root.join(id);
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
    pub keyring: Option<&'a Path>,
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
    if options.write && options.keyring.is_none() && !options.allow_unsigned_dev {
        return Err(Error::Invalid(
            "apply --write needs --keyring or explicit --allow-unsigned-dev".into(),
        ));
    }
    let index = soil_store::Index::read(options.index)?;
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
        let state = install(
            &package,
            root,
            InstallOptions {
                keyring: options.keyring,
                key: None,
                publisher: entry.and_then(|entry| entry.publisher.as_deref()),
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
            signature: String::new(),
        };
        entry.seq = 1;
        let index = soil_store::Index::seal("test", vec![entry]);
        let index_path = temp.path().join("index.json");
        std::fs::write(&index_path, serde_json::to_vec(&index).unwrap()).unwrap();

        let outcomes = apply(
            &root,
            ApplyOptions {
                index: &index_path,
                packages: &packages,
                keyring: None,
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
