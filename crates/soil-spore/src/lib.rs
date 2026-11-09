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
use std::collections::BTreeMap;
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
    /// The Lua entrypoint.
    ///
    /// Optional, and a first-class entrypoint in its own right: the host JITs
    /// this file, so a Spore does not need a wasm build to exist. A package
    /// with neither a module nor a UI nor a chrome region does nothing, and
    /// validation refuses it.
    #[serde(default)]
    pub ui: Option<Ui>,
    pub launch: Launch,
    /// Build-time dependencies and the exact version each was built against.
    ///
    /// A composed artifact pins its imports by construction, so a range would be
    /// a promise the bytes cannot keep. Recording the exact version is what lets
    /// a reviewer see which SDK produced the artifact, and lets a builder refuse
    /// to compose against a different one.
    #[serde(default)]
    pub deps: BTreeMap<String, String>,
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
    /// Composed Lua for a region, when the region's artifact is not the Spore's
    /// own `[ui] entry`.
    ///
    /// The common case is one region and one UI file, so this is usually empty:
    /// repeating `[ui] entry` under `[chrome.sources]` only creates two places
    /// that can disagree. It stays for the package that ships a distinct
    /// composed artifact per region. The host still does not compose at runtime;
    /// whichever path wins is what the lock covers.
    #[serde(default)]
    pub sources: BTreeMap<String, String>,
    /// Logical height this package authors the region at.
    ///
    /// Absent means the host's default. The number is declared rather than
    /// inferred because the host has to reserve the region *before* the package
    /// runs: it draws the built-in fallback at this height, then hands over. A
    /// package that disagrees with the fallback is a package that shifts the
    /// desktop when it loads, which is why the shell reads this instead of
    /// carrying a constant of its own.
    #[serde(default)]
    pub height: Option<u32>,
}

impl Chrome {
    /// The height the host reserves for this package's region.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height.unwrap_or(chrome::DEFAULT_HEIGHT)
    }
}

/// Shell metrics that belong to the region contract rather than to a renderer.
///
/// These lived in a graphics crate, which made the shell depend on a drawing
/// library to know how tall its own bar is. A chrome package may narrow them in
/// its manifest; the host uses the same numbers for the fallback it draws before
/// any package loads.
pub mod chrome {
    /// Logical height of the bar region.
    pub const DEFAULT_HEIGHT: u32 = 44;
    /// Outer padding inside the bar.
    pub const DEFAULT_PADDING: u32 = 8;
    /// Label size in the bar, in logical pixels.
    pub const DEFAULT_TEXT_SIZE: u32 = 14;
    /// Height of an interactive control in the bar; the whole row, so the target
    /// is the row rather than the glyph.
    pub const DEFAULT_CONTROL_HEIGHT: u32 = DEFAULT_HEIGHT;
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
            // A Spore without a module still has to do something. Any entrypoint
            // counts, and Lua is one of them: the host JITs `[ui] entry`, so a
            // Lua-only Spore is a program, not a second-class attachment to a
            // wasm build. `chrome` counts because a host draws the region.
            None if self.ui.is_none() && self.chrome.is_none() => {
                return Err(Error::Invalid(
                    "a Spore needs an entrypoint: a module, a Lua UI, or a chrome region".into(),
                ));
            }
            None => {}
        }
        if let Some(ui) = &self.ui {
            safe_relative(&ui.entry)?;
            if !ui.entry.ends_with(".lua") {
                return Err(Error::Invalid("ui.entry must end in .lua".into()));
            }
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

        for (name, version) in &self.deps {
            if !identifier(name) {
                return Err(Error::Invalid(format!(
                    "dependency {name:?} must be a lowercase identifier such as `fungos.ui`"
                )));
            }
            if !version_is_exact(version) {
                return Err(Error::Invalid(format!(
                    "dependency {name:?} = {version:?} must be one exact version, not a range: a composed artifact cannot float"
                )));
            }
        }

        for capability in self.grants.keys() {
            if !grants(capability) {
                return Err(Error::Invalid(format!(
                    "grant {capability:?} is not a soil:/wasi:/fungos: capability"
                )));
            }
        }
        if let Some(chrome) = &self.chrome {
            if chrome.regions.is_empty() {
                return Err(Error::Invalid("chrome needs at least one region".into()));
            }
            for region in &chrome.regions {
                if !CHROME_REGIONS.contains(&region.as_str()) {
                    return Err(Error::Invalid(format!(
                        "chrome region {region:?} is not one of {CHROME_REGIONS:?}"
                    )));
                }
                let Some(source) = self.region_source(region) else {
                    return Err(Error::Invalid(format!(
                        "chrome region {region:?} has no source: give the Spore a [ui] entry or name one in [chrome.sources]"
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
        let mut paths = Vec::new();
        if let Some(ui) = &self.ui {
            paths.push(ui.entry.clone());
        }
        if let Some(module) = &self.module {
            paths.push(module.path.clone());
        }
        if let Some(chrome) = &self.chrome {
            for region in &chrome.regions {
                if let Some(source) = chrome.sources.get(region) {
                    paths.push(source.clone());
                }
            }
        }
        paths.extend(self.assets.values().map(|asset| asset.path.clone()));
        let mut seen = std::collections::BTreeSet::new();
        paths
            .into_iter()
            .filter(|path| safe_relative(path).is_ok())
            .map(PathBuf::from)
            .filter(|path| seen.insert(path.clone()))
            .collect()
    }

    /// The Lua a host loads for a chrome `region`.
    ///
    /// An explicit per-region artifact wins; otherwise the Spore's single `[ui]
    /// entry` is the region's Lua. One region plus one UI file is the common
    /// shape, and naming the same path twice only creates two places to disagree.
    #[must_use]
    pub fn region_source(&self, region: &str) -> Option<&str> {
        let chrome = self.chrome.as_ref()?;
        if !chrome.regions.iter().any(|claimed| claimed == region) {
            return None;
        }
        chrome
            .sources
            .get(region)
            .map(String::as_str)
            .or_else(|| self.ui.as_ref().map(|ui| ui.entry.as_str()))
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

/// One exact version: no `^`, `~`, `>=`, `*` or range separators.
fn version_is_exact(value: &str) -> bool {
    version(value) && !value.contains(['^', '~', '*', '>', '<', ' ', ','])
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
    let (manifest, lock, _) = read_package(bytes)?;
    Ok((manifest, lock))
}

/// Unpack a verified package into `dest`.
///
/// The archive is the distribution unit: a machine installs from a `.spore`
/// file and never needs the Spore's source, its composer, or the SDK it was
/// built against. Everything is verified before a single byte is written, so a
/// tampered or partial package cannot leave half a Spore on disk.
///
/// # Errors
/// Fails when the package is unsafe, tampered, or the destination cannot be
/// written.
pub fn unpack(bytes: &[u8], dest: &Path) -> Result<Lock> {
    let (_, lock, files) = read_package(bytes)?;
    for (path, body) in files {
        let target = dest.join(&path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        fs::write(&target, body).map_err(io_error)?;
    }
    Ok(lock)
}

/// The CIDv1 of a package's bytes: raw codec over sha2-256.
///
/// A `.spore` tar is deterministic, so the same tree yields the same bytes and
/// therefore the same CID. That is the identity a registry versions on top of;
/// `spore.lock` stays the human-readable digest.
#[must_use]
pub fn cid_v1_raw(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut block = vec![0x01, 0x55, 0x12, 0x20];
    block.extend_from_slice(&digest);
    format!("b{}", base32_lower(&block))
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

fn read_package(bytes: &[u8]) -> Result<(Manifest, Lock, BTreeMap<String, Vec<u8>>)> {
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
    Ok((manifest, lock, files))
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
        with_ui("[ui]\nentry = \"ui/main.lua\"\n", extra)
    }

    /// Same manifest with the `[ui]` block chosen by the caller, so a test can
    /// ask what a package looks like with no Lua entrypoint at all.
    fn with_ui(ui: &str, extra: &str) -> String {
        format!(
            "schema = 1\n[spore]\nid = \"dev.fungos.chrome\"\nname = \"chrome\"\nversion = \"0.1.0\"\nlicense = \"MIT\"\nsummary = \"shell chrome\"\n[engine]\nsoil = \">=0.1\"\ntarget = \"wasm32-wasip2\"\n[module]\npath = \"module.wasm\"\n{ui}[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n{extra}"
        )
    }

    #[test]
    fn a_chrome_spore_must_source_every_region_it_claims() {
        // One region, one UI file: the entry already is the region's Lua, so no
        // `[chrome.sources]` is needed and none can go stale.
        let implicit = Manifest::parse(&manifest("[chrome]\nregions = [\"dock\"]\n"))
            .expect("a single-region Spore defaults to its UI entry");
        assert_eq!(implicit.region_source("dock"), Some("ui/main.lua"));
        assert_eq!(
            implicit.region_source("top-bar"),
            None,
            "an unclaimed region has no source"
        );

        let explicit = Manifest::parse(&manifest(
            "[chrome]\nregions = [\"dock\"]\n[chrome.sources]\ndock = \"dist/dock.lua\"\n[presentation]\nbadge = \"D\"\ncolor = \"#8bd5ca\"\n",
        ))
        .expect("an explicit region artifact wins over the UI entry");
        assert_eq!(explicit.region_source("dock"), Some("dist/dock.lua"));
        assert_eq!(explicit.presentation.as_ref().unwrap().badge, "D");

        let missing = Manifest::parse(&with_ui("", "[chrome]\nregions = [\"dock\"]\n"));
        assert!(
            missing.is_err(),
            "a region with no source and no UI entry is a hole in the shell"
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
        let explicit = Manifest::parse(&manifest(
            "[chrome]\nregions = [\"dock\"]\n[chrome.sources]\ndock = \"dist/dock.lua\"\n",
        ))
        .expect("manifest");
        let paths = explicit.declared_paths();
        assert!(paths.contains(&PathBuf::from("dist/dock.lua")));

        let implicit =
            Manifest::parse(&manifest("[chrome]\nregions = [\"dock\"]\n")).expect("manifest");
        assert_eq!(
            implicit.declared_paths(),
            vec![PathBuf::from("ui/main.lua"), PathBuf::from("module.wasm")],
            "the UI entry is declared once even when a region also resolves to it"
        );
    }

    #[test]
    fn a_dependency_is_one_exact_version() {
        let ok = Manifest::parse(&manifest("[deps]\n\"fungos.ui\" = \"0.1.0\"\n"));
        let ok = ok.expect("an exact version parses");
        assert_eq!(ok.deps.get("fungos.ui").map(String::as_str), Some("0.1.0"));

        for range in ["^0.1.0", "~0.1.0", ">=0.1.0", "0.1.*", "<0.2.0"] {
            let bad = Manifest::parse(&manifest(&format!("[deps]\n\"fungos.ui\" = \"{range}\"\n")));
            assert!(
                bad.is_err(),
                "{range} is a range, and a composed artifact cannot float"
            );
        }

        let bad_name = Manifest::parse(&manifest("[deps]\n\"Fungos.UI\" = \"0.1.0\"\n"));
        assert!(
            bad_name.is_err(),
            "a dependency name is a lowercase identifier"
        );
    }

    #[test]
    fn lua_is_an_entrypoint_of_its_own() {
        let lua_only = with_ui("[ui]\nentry = \"ui/app.lua\"\n", "")
            .replace("[module]\npath = \"module.wasm\"\n", "");
        let parsed = Manifest::parse(&lua_only).expect("a Lua-only Spore is a Spore");
        assert!(parsed.module.is_none());
        assert_eq!(
            parsed.declared_paths(),
            vec![PathBuf::from("ui/app.lua")],
            "no wasm means nothing claims a module file"
        );

        // Chrome with no UI entry still installs, because the host draws it and
        // the region names its own artifact.
        let chrome_only = Manifest::parse(
            &with_ui("", "[chrome]\nregions = [\"dock\"]\n")
                .replace("[module]\npath = \"module.wasm\"\n", "")
                .replace(
                    "[chrome]\nregions = [\"dock\"]\n",
                    "[chrome]\nregions = [\"dock\"]\n[chrome.sources]\ndock = \"ui/dock.lua\"\n",
                ),
        )
        .expect("a chrome Spore needs no UI entry when it names its artifact");
        assert_eq!(chrome_only.region_source("dock"), Some("ui/dock.lua"));
    }

    #[test]
    fn a_spore_with_no_entrypoint_is_refused() {
        let empty = with_ui("", "").replace("[module]\npath = \"module.wasm\"\n", "");
        let error = Manifest::parse(&empty).expect_err("nothing to run, nothing to draw");
        assert!(
            error.to_string().contains("entrypoint"),
            "the error says what is missing: {error}"
        );
    }
}
