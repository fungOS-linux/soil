//! Spore package format: manifest, lock, deterministic tar, PGP sidecar.
//!
//! A Spore is a module (WASM), a UI (Lua), and a manifest that names every
//! asset and grant. Soil builds and verifies the package. It does not publish,
//! host, or resolve: distribution is a tar over IPFS, and trust roots come from
//! Mycelium, which binds a PGP key to a user or accepts an external keyring.
//!
//! ```text
//! markdown/
//!   spore.toml
//!   module.wasm
//!   ui/editor.lua
//!   assets/site-map.png
//!   signatures/<keyid>.asc   (excluded from the lock and the tar)
//!   spore.lock               (generated, included in the tar)
//! ```
//!
//! The lock is the package identity: sorted `sha256 size path` lines plus a
//! digest over that canonical text. Sign the tar; verify the tar, then the lock.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

pub mod publisher;

pub const SCHEMA: u32 = 1;
pub const LOCK_NAME: &str = "spore.lock";
pub const SIGNATURE_DIR: &str = "signatures";
pub const LOCK_HEADER: &str = "spore-lock 1";

/// Largest package Soil will read or write.
pub const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;

/// Largest single file inside a package.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Io(String),
    Toml(String),
    Invalid(String),
    UnsafePath(String),
    Lock(String),
    Signature(String),
    Tool(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "io: {message}"),
            Self::Toml(message) => write!(f, "manifest: {message}"),
            Self::Invalid(message) => write!(f, "invalid spore: {message}"),
            Self::UnsafePath(path) => write!(f, "unsafe path: {path}"),
            Self::Lock(message) => write!(f, "lock: {message}"),
            Self::Signature(message) => write!(f, "signature: {message}"),
            Self::Tool(message) => write!(f, "tool: {message}"),
        }
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

// ---- manifest -------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub spore: SporeMeta,
    pub engine: Engine,
    /// Absent for a Spore with no semantics of its own -- chrome, for instance:
    /// a dock is a composition, not a program.
    #[serde(default)]
    pub module: Option<Module>,
    pub ui: Ui,
    pub launch: Launch,
    #[serde(default)]
    pub grants: BTreeMap<String, String>,
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
    /// Declared absences. Reviewed like grants: intent is written down.
    #[serde(default)]
    pub denied: BTreeMap<String, Vec<String>>,
    /// Declares a host-system surface. The host must still allowlist the exact
    /// Spore id and signed digest; this flag is not a grant.
    #[serde(default)]
    pub system: Option<System>,
    /// Chrome regions this Spore draws, when it draws any.
    #[serde(default)]
    pub chrome: Option<Chrome>,
    /// Launcher identity: a mark and the colour it is drawn in.
    #[serde(default)]
    pub presentation: Option<Presentation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct System {
    pub role: String,
}

/// A Spore that draws a slice of the shell instead of, or as well as, a window.
///
/// Chrome is a region, not a window: the host owns the extent, the input routing
/// and the fallback. A chrome Spore that fails to render must never leave a hole
/// in the desktop, so every region it claims has a built-in implementation that
/// takes over.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chrome {
    /// `top-bar`, `dock`, `control-center`.
    pub regions: Vec<String>,
    /// Composed Lua for each region, already single-file: the host does not
    /// compose at runtime, because composition is a build step that produces an
    /// artifact the lock can cover.
    pub sources: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    /// One or two characters. A monogram needs no asset and cannot go missing.
    pub badge: String,
    /// `#rrggbb`, the app's own colour rather than a theme role.
    pub color: String,
}

/// Regions a Spore may claim. An unlisted name is a typo or a claim the host
/// cannot honour, and both are better refused at install than discovered later.
pub const CHROME_REGIONS: [&str; 7] = [
    "top-bar",
    "dock",
    "launcher",
    "switcher",
    "control-center",
    // Transient surfaces: the shell's own voice. Kept as regions rather than
    // windows so the host still owns where they appear and when they dismiss.
    "notifications",
    "attention",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SporeMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub summary: String,
    #[serde(default)]
    pub homepage: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engine {
    pub soil: String,
    pub target: String,
    #[serde(default)]
    pub abi: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub path: String,
    /// Filled by `lock` when absent; checked when present.
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    pub entry: String,
    #[serde(default = "default_true")]
    pub jit: bool,
    #[serde(default = "default_true")]
    pub hot_reload: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub default: String,
    pub surfaces: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub path: String,
    pub media_type: String,
}

const fn default_true() -> bool {
    true
}

pub const SURFACES: [&str; 3] = ["web", "native", "canvas"];

impl Manifest {
    pub fn parse(source: &str) -> Result<Self> {
        let manifest: Self =
            toml::from_str(source).map_err(|error| Error::Toml(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("spore.toml");
        let source = fs::read_to_string(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        Self::parse(&source)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::Invalid(format!(
                "schema {} is not {SCHEMA}",
                self.schema
            )));
        }
        if !identifier(&self.spore.id) || !self.spore.id.contains('.') {
            return Err(Error::Invalid(format!(
                "id {:?} must be a stable reverse-DNS identifier",
                self.spore.id
            )));
        }
        for (label, value) in [
            ("name", &self.spore.name),
            ("version", &self.spore.version),
            ("license", &self.spore.license),
            ("summary", &self.spore.summary),
        ] {
            if value.trim().is_empty() {
                return Err(Error::Invalid(format!("{label} must not be empty")));
            }
        }
        if !version(&self.spore.version) {
            return Err(Error::Invalid(format!(
                "version {:?} is not semver-ish",
                self.spore.version
            )));
        }

        match &self.module {
            Some(module) => {
                safe_relative(&module.path)?;
                if !module.path.ends_with(".wasm") {
                    return Err(Error::Invalid("module.path must end in .wasm".into()));
                }
            }
            // A Spore with no module must still do something: chrome draws a
            // region, and nothing else is worth installing without semantics.
            None if self.chrome.is_none() => {
                return Err(Error::Invalid(
                    "a Spore needs a module unless it draws chrome".into(),
                ));
            }
            None => {}
        }
        safe_relative(&self.ui.entry)?;
        if !self.ui.entry.ends_with(".lua") {
            return Err(Error::Invalid("ui.entry must end in .lua".into()));
        }

        if self.launch.surfaces.is_empty() {
            return Err(Error::Invalid("launch.surfaces must not be empty".into()));
        }
        for surface in &self.launch.surfaces {
            if !SURFACES.contains(&surface.as_str()) {
                return Err(Error::Invalid(format!("unknown surface {surface:?}")));
            }
        }
        if !self.launch.surfaces.contains(&self.launch.default) {
            return Err(Error::Invalid(format!(
                "launch.default {:?} is not in launch.surfaces",
                self.launch.default
            )));
        }

        if let Some(system) = &self.system
            && !identifier(&system.role)
        {
            return Err(Error::Invalid(format!(
                "system role {:?} is not an identifier",
                system.role
            )));
        }

        for capability in self.grants.keys() {
            if !grants(capability) {
                return Err(Error::Invalid(format!(
                    "grant {capability:?} is not a soil:/wasi:/fungos: capability"
                )));
            }
        }
        if let Some(chrome) = &self.chrome {
            if chrome.regions.is_empty() || chrome.sources.is_empty() {
                return Err(Error::Invalid(
                    "chrome needs at least one region, and sources to match".into(),
                ));
            }
            for region in &chrome.regions {
                if !CHROME_REGIONS.contains(&region.as_str()) {
                    return Err(Error::Invalid(format!(
                        "chrome region {region:?} is not one of {CHROME_REGIONS:?}"
                    )));
                }
                let Some(source) = chrome.sources.get(region) else {
                    return Err(Error::Invalid(format!(
                        "chrome region {region:?} has no source"
                    )));
                };
                safe_relative(source)?;
                if !source.ends_with(".lua") {
                    return Err(Error::Invalid(format!(
                        "chrome source {source:?} must be a .lua artifact"
                    )));
                }
            }
        }
        if let Some(presentation) = &self.presentation {
            let glyphs = presentation.badge.chars().count();
            if !(1..=2).contains(&glyphs) {
                return Err(Error::Invalid(
                    "presentation.badge must be one or two characters".into(),
                ));
            }
            let hex = presentation.color.trim_start_matches('#');
            if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(Error::Invalid("presentation.color must be #rrggbb".into()));
            }
        }
        for (name, asset) in &self.assets {
            if !identifier(name) {
                return Err(Error::Invalid(format!("asset name {name:?}")));
            }
            safe_relative(&asset.path)?;
            if asset.media_type.trim().is_empty() || !asset.media_type.contains('/') {
                return Err(Error::Invalid(format!("asset {name:?} needs a media type")));
            }
        }
        Ok(())
    }

    /// Every file the manifest declares explicitly.
    pub fn declared_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.ui.entry.clone()];
        if let Some(module) = &self.module {
            paths.push(module.path.clone());
        }
        if let Some(chrome) = &self.chrome {
            paths.extend(chrome.sources.values().cloned());
        }
        paths.extend(self.assets.values().map(|asset| asset.path.clone()));
        paths
            .into_iter()
            .filter_map(|path| safe_relative(&path).ok())
            .collect()
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn version(value: &str) -> bool {
    let core = value.split(['-', '+']).next().unwrap_or(value);
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn grants(value: &str) -> bool {
    let Some((package, interface)) = value.split_once(':') else {
        return false;
    };
    matches!(package, "soil" | "wasi" | "fungos")
        && !interface.is_empty()
        && interface.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_' | '/')
        })
}

/// Resolve a manifest path without letting it leave the package.
pub fn safe_relative(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.len() > 255 || path.starts_with('/') || path.contains('\\') {
        return Err(Error::UnsafePath(path.to_owned()));
    }
    if path.chars().any(char::is_control) {
        return Err(Error::UnsafePath(path.to_owned()));
    }
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return Err(Error::UnsafePath(path.to_owned())),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(Error::UnsafePath(path.to_owned()));
    }
    Ok(out)
}

// ---- lock -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lock {
    pub id: String,
    pub version: String,
    pub files: Vec<FileEntry>,
    pub digest: String,
}

impl Lock {
    pub fn new(id: &str, version: &str, mut files: Vec<FileEntry>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let body = body(id, version, &files);
        Self {
            id: id.to_owned(),
            version: version.to_owned(),
            files,
            digest: hex(&Sha256::digest(body.as_bytes())),
        }
    }

    /// Canonical text, including the digest line.
    pub fn render(&self) -> String {
        let mut out = body(&self.id, &self.version, &self.files);
        out.push_str(&format!("digest {}\n", self.digest));
        out
    }

    pub fn parse(source: &str) -> Result<Self> {
        let mut id = None;
        let mut version = None;
        let mut files = Vec::new();
        let mut digest = None;

        for line in source.lines() {
            if line.is_empty() || line == LOCK_HEADER {
                continue;
            }
            let (key, rest) = line
                .split_once(' ')
                .ok_or_else(|| Error::Lock(format!("malformed line {line:?}")))?;
            match key {
                "spore" => id = Some(rest.to_owned()),
                "version" => version = Some(rest.to_owned()),
                "digest" => digest = Some(rest.to_owned()),
                "file" => {
                    let mut parts = rest.splitn(3, ' ');
                    let sha256 = parts.next().unwrap_or_default();
                    let size = parts.next().unwrap_or_default();
                    let path = parts.next().unwrap_or_default();
                    if sha256.len() != 64
                        || !sha256.chars().all(|c| c.is_ascii_hexdigit())
                        || path.is_empty()
                    {
                        return Err(Error::Lock(format!("malformed file line {line:?}")));
                    }
                    files.push(FileEntry {
                        path: path.to_owned(),
                        sha256: sha256.to_ascii_lowercase(),
                        size: size
                            .parse()
                            .map_err(|_| Error::Lock(format!("bad size {size:?}")))?,
                    });
                }
                other => return Err(Error::Lock(format!("unknown field {other:?}"))),
            }
        }

        let id = id.ok_or_else(|| Error::Lock("missing spore".into()))?;
        let version = version.ok_or_else(|| Error::Lock("missing version".into()))?;
        let expected = digest.ok_or_else(|| Error::Lock("missing digest".into()))?;
        let lock = Self::new(&id, &version, files);
        if lock.digest != expected {
            return Err(Error::Lock(format!(
                "digest mismatch: declared {expected}, computed {}",
                lock.digest
            )));
        }
        Ok(lock)
    }

    pub fn file(&self, path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|entry| entry.path == path)
    }
}

fn body(id: &str, version: &str, files: &[FileEntry]) -> String {
    let mut out = String::new();
    out.push_str(LOCK_HEADER);
    out.push('\n');
    out.push_str(&format!("spore {id}\n"));
    out.push_str(&format!("version {version}\n"));
    for file in files {
        out.push_str(&format!(
            "file {} {} {}\n",
            file.sha256, file.size, file.path
        ));
    }
    out
}

// ---- package reading ------------------------------------------------------

/// Every package file except `signatures/` and `spore.lock`, sorted by path.
pub fn collect(root: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = BTreeMap::new();
    walk(root, root, &mut files)?;
    Ok(files.into_iter().collect())
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    let entries =
        fs::read_dir(dir).map_err(|error| Error::Io(format!("{}: {error}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::Io(error.to_string()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        if metadata.file_type().is_symlink() {
            return Err(Error::UnsafePath(format!(
                "{} is a symlink; packages contain regular files",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| Error::UnsafePath(path.display().to_string()))?;
        let name = relative
            .to_str()
            .ok_or_else(|| Error::UnsafePath(relative.display().to_string()))?
            .replace('\\', "/");

        if metadata.is_dir() {
            if name == SIGNATURE_DIR {
                continue;
            }
            walk(root, &path, out)?;
            continue;
        }
        if !metadata.is_file() || name == LOCK_NAME {
            continue;
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Error::Invalid(format!(
                "{name} exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        let bytes =
            fs::read(&path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        out.insert(name, bytes);
    }
    Ok(())
}

/// Compute the lock for a directory's current contents.
pub fn compute_lock(root: &Path, manifest: &Manifest) -> Result<Lock> {
    let files = collect(root)?;
    Ok(lock_of(&manifest.spore.id, &manifest.spore.version, &files))
}

fn lock_of(id: &str, version: &str, files: &[(String, Vec<u8>)]) -> Lock {
    let entries = files
        .iter()
        .map(|(path, bytes)| FileEntry {
            path: path.clone(),
            sha256: hex(&Sha256::digest(bytes)),
            size: bytes.len() as u64,
        })
        .collect();
    Lock::new(id, version, entries)
}

/// Check a directory: manifest, declared files, and the lock if present.
/// The WIT packages a component imports, e.g. `wasi:http`, `soil:records`.
///
/// A package is the unit a manifest declares, so this is what `[grants]` must
/// cover: an import nobody declared is a capability the package author did not
/// write down.
pub fn imported_packages(bytes: &[u8]) -> Result<BTreeSet<String>> {
    // `wit_component::decode` also accepts a bare core module and reports an
    // empty world for it, which would make the manifest rule vacuous. A Spore
    // module is a component by definition, so check the preamble first: a
    // component starts `\0asm\x0d\x00`, a core module `\0asm\x01\x00`.
    if !(bytes.len() >= 8 && &bytes[..4] == b"\0asm" && bytes[4] == 0x0d && bytes[5] == 0x00) {
        return Err(Error::Invalid(
            "module is a core wasm module, not a component".into(),
        ));
    }
    let decoded = wit_component::decode(bytes)
        .map_err(|error| Error::Invalid(format!("module is not a component: {error}")))?;
    let (resolve, world) = match decoded {
        wit_component::DecodedWasm::Component(resolve, world) => (resolve, world),
        wit_component::DecodedWasm::WitPackage(..) => {
            return Err(Error::Invalid(
                "module is a WIT package, not a component".into(),
            ));
        }
    };
    let mut packages = BTreeSet::new();
    for key in resolve.worlds[world].imports.keys() {
        let wit_parser::WorldKey::Interface(id) = key else {
            continue;
        };
        let Some(package) = resolve.interfaces[*id].package else {
            continue;
        };
        let name = &resolve.packages[package].name;
        packages.insert(format!("{}:{}", name.namespace, name.name));
    }
    Ok(packages)
}

fn undeclared_packages(
    imported: &BTreeSet<String>,
    grants: &BTreeMap<String, String>,
) -> Vec<String> {
    imported
        .iter()
        .filter(|package| !grants.contains_key(package.as_str()))
        .cloned()
        .collect()
}

/// Run the module's real imports past the manifest.
///
/// The manifest is intent; this makes it checkable. A module that imports
/// `soil:records` while the manifest lists only WASI fails here instead of at
/// review time.
fn check_declared_grants(root: &Path, manifest: &Manifest) -> Result<()> {
    // A chrome-only Spore has no module, so there is nothing to import.
    let Some(module) = &manifest.module else {
        return Ok(());
    };
    let path = root.join(&module.path);
    let bytes =
        fs::read(&path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    let imported = imported_packages(&bytes)?;
    let undeclared = undeclared_packages(&imported, &manifest.grants);
    if !undeclared.is_empty() {
        return Err(Error::Invalid(format!(
            "{} imports {} but [grants] does not declare {}",
            module.path,
            undeclared.join(", "),
            if undeclared.len() == 1 { "it" } else { "them" },
        )));
    }
    Ok(())
}

pub fn check(root: &Path) -> Result<Lock> {
    let manifest = Manifest::load(root)?;
    for path in manifest.declared_paths() {
        let full = root.join(&path);
        if !full.is_file() {
            return Err(Error::Invalid(format!(
                "{} is declared but missing",
                path.display()
            )));
        }
    }
    check_declared_grants(root, &manifest)?;
    let lock = compute_lock(root, &manifest)?;
    let path = root.join(LOCK_NAME);
    if path.is_file() {
        let declared = fs::read_to_string(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        let declared = Lock::parse(&declared)?;
        if declared != lock {
            return Err(Error::Lock(format!(
                "{LOCK_NAME} is stale; run `soil-spore lock`"
            )));
        }
    }
    Ok(lock)
}

/// Write a fresh lock.
pub fn write_lock(root: &Path) -> Result<Lock> {
    let manifest = Manifest::load(root)?;
    let lock = compute_lock(root, &manifest)?;
    let path = root.join(LOCK_NAME);
    fs::write(&path, lock.render())
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    Ok(lock)
}

// ---- tar ------------------------------------------------------------------

/// Pack a directory into a deterministic tar.
///
/// Sorted entries, zero mtime/uid/gid, fixed modes. Repacking the same tree
/// yields identical bytes, so an IPFS CID identifies the package and nothing
/// else.
pub fn pack(root: &Path) -> Result<Vec<u8>> {
    let manifest = Manifest::load(root)?;
    let lock = write_lock(root)?;
    if lock.id != manifest.spore.id || lock.version != manifest.spore.version {
        return Err(Error::Lock("lock does not match the manifest".into()));
    }

    let mut files = collect(root)?;
    files.push((LOCK_NAME.to_owned(), lock.render().into_bytes()));

    let total: u64 = files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    if total > MAX_PACKAGE_BYTES {
        return Err(Error::Invalid(format!(
            "package exceeds {MAX_PACKAGE_BYTES} bytes"
        )));
    }

    let mut builder = tar::Builder::new(Vec::new());
    for (path, bytes) in &files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_username("").map_err(io_error)?;
        header.set_groupname("").map_err(io_error)?;
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(bytes))
            .map_err(io_error)?;
    }
    builder.into_inner().map_err(io_error)
}

/// Verify a packed tar: entry safety, embedded lock, and every digest.
pub fn verify(bytes: &[u8]) -> Result<(Manifest, Lock)> {
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err(Error::Invalid("package is too large".into()));
    }
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive.entries().map_err(io_error)?;

    for entry in entries {
        let mut entry = entry.map_err(io_error)?;
        let kind = entry.header().entry_type();
        if !kind.is_file() {
            return Err(Error::UnsafePath(format!(
                "{} is not a regular file",
                entry
                    .path()
                    .map_or_else(|_| "entry".into(), |p| p.display().to_string())
            )));
        }
        let path = entry
            .path()
            .map_err(io_error)?
            .to_str()
            .ok_or_else(|| Error::UnsafePath("entry name is not UTF-8".into()))?
            .replace('\\', "/");
        safe_relative(&path)?;
        if files.contains_key(&path) {
            return Err(Error::Invalid(format!("duplicate entry {path}")));
        }
        let mut buffer = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buffer).map_err(io_error)?;
        files.insert(path, buffer);
    }

    let lock_bytes = files
        .remove(LOCK_NAME)
        .ok_or_else(|| Error::Lock(format!("package has no {LOCK_NAME}")))?;
    let lock = Lock::parse(
        std::str::from_utf8(&lock_bytes)
            .map_err(|_| Error::Lock("spore.lock is not UTF-8".into()))?,
    )?;

    let computed = lock_of(
        &lock.id,
        &lock.version,
        &files
            .iter()
            .map(|(path, bytes)| (path.clone(), bytes.clone()))
            .collect::<Vec<_>>(),
    );
    if computed.digest != lock.digest {
        return Err(Error::Lock(format!(
            "contents do not match the lock (expected {}, got {})",
            lock.digest, computed.digest
        )));
    }

    let manifest = Manifest::parse(
        std::str::from_utf8(
            files
                .get("spore.toml")
                .ok_or_else(|| Error::Invalid("package has no spore.toml".into()))?,
        )
        .map_err(|_| Error::Invalid("spore.toml is not UTF-8".into()))?,
    )?;
    if manifest.spore.id != lock.id || manifest.spore.version != lock.version {
        return Err(Error::Lock(
            "manifest identity does not match the lock".into(),
        ));
    }
    Ok((manifest, lock))
}

// ---- signatures -----------------------------------------------------------

/// Sign a packed tar with a detached, armored PGP signature.
///
/// Soil never generates, stores, or interprets a key. Mycelium binds a key to a
/// user and supplies the identity; an external keyring is equally acceptable.
pub fn sign(package: &Path, signature: &Path, key: Option<&str>) -> Result<()> {
    let mut command = std::process::Command::new("gpg");
    command
        .arg("--batch")
        .arg("--yes")
        .arg("--armor")
        .arg("--detach-sign")
        .arg("--output")
        .arg(signature);
    if let Some(key) = key {
        command.arg("--local-user").arg(key);
    }
    command.arg(package);
    run(command, "gpg --detach-sign")
}

/// Verify a detached signature against a trusted keyring.
pub fn verify_signature(
    package: &Path,
    signature: &Path,
    keyring: Option<&Path>,
    required_key: Option<&str>,
) -> Result<()> {
    let mut command = std::process::Command::new("gpg");
    command
        .arg("--batch")
        .arg("--no-tty")
        .arg("--status-fd")
        .arg("1");
    if let Some(keyring) = keyring {
        command
            .arg("--no-default-keyring")
            .arg("--keyring")
            .arg(keyring);
    }
    command.arg("--verify").arg(signature).arg(package);

    let output = command
        .output()
        .map_err(|error| Error::Tool(format!("gpg: {error}")))?;
    if !output.status.success() {
        return Err(Error::Signature(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    if let Some(key) = required_key {
        let status = String::from_utf8_lossy(&output.stdout);
        let valid = status.lines().any(|line| {
            line.contains("VALIDSIG") && line.split_whitespace().any(|field| field.ends_with(key))
        });
        if !valid {
            return Err(Error::Signature(format!("not signed by {key}")));
        }
    }
    Ok(())
}

fn run(mut command: std::process::Command, label: &str) -> Result<()> {
    let output = command
        .output()
        .map_err(|error| Error::Tool(format!("{label}: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Tool(format!(
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn io_error(error: impl fmt::Display) -> Error {
    Error::Io(error.to_string())
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod chrome_tests {
    use super::*;

    fn manifest(extra: &str) -> String {
        format!(
            "schema = 1\n[spore]\nid = \"dev.fungos.chrome\"\nname = \"chrome\"\nversion = \"0.1.0\"\nlicense = \"MIT\"\nsummary = \"shell chrome\"\n[engine]\nsoil = \">=0.1\"\ntarget = \"wasm32-wasip2\"\n[module]\npath = \"module.wasm\"\n[ui]\nentry = \"ui/main.lua\"\n[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n{extra}"
        )
    }

    #[test]
    fn a_chrome_spore_must_source_every_region_it_claims() {
        let ok = Manifest::parse(&manifest(
            "[chrome]\nregions = [\"dock\"]\n[chrome.sources]\ndock = \"ui/dock.lua\"\n[presentation]\nbadge = \"D\"\ncolor = \"#8bd5ca\"\n",
        ))
        .expect("a sourced region parses");
        assert_eq!(ok.chrome.as_ref().unwrap().regions, vec!["dock"]);
        assert_eq!(ok.presentation.as_ref().unwrap().badge, "D");

        let missing = Manifest::parse(&manifest("[chrome]\nregions = [\"dock\"]\n"));
        assert!(
            missing.is_err(),
            "a region with no source is a hole in the shell"
        );

        let unknown = Manifest::parse(&manifest(
            "[chrome]\nregions = [\"statusbar\"]\n[chrome.sources]\nstatusbar = \"ui/x.lua\"\n",
        ));
        assert!(unknown.is_err(), "the host only has the regions it knows");

        let bad_color = Manifest::parse(&manifest(
            "[presentation]\nbadge = \"D\"\ncolor = \"teal\"\n",
        ));
        assert!(bad_color.is_err(), "a colour is #rrggbb or it is nothing");
    }

    #[test]
    fn declared_paths_cover_chrome_sources() {
        let manifest = Manifest::parse(&manifest(
            "[chrome]\nregions = [\"dock\"]\n[chrome.sources]\ndock = \"ui/dock.lua\"\n",
        ))
        .expect("manifest");
        let paths = manifest.declared_paths();
        assert!(paths.contains(&PathBuf::from("ui/dock.lua")));
    }
}
