//! The host side of `soil:spore.catalog`.
//!
//! A Spore never scans the filesystem or chooses a window. This is the half that
//! does: it reads the install root, answers `entries()` with what this host
//! actually has, and answers `launch()` by resolving a declared surface against
//! what this host can present.
//!
//! The types mirror `wit/soil-spore.wit` field for field. They are Rust structs
//! rather than generated bindings because the linking step (a wasmtime host
//! implementation of the interface) is a separate, heavier change; keeping the
//! shape here means that step is mechanical rather than a second design.
//!
//! Two rules come straight from the WIT:
//!
//! * An `entry` names a **surface**, and `entry` is a path inside the package.
//!   The package's digest already covers every byte under its root, so a
//!   per-asset content id would be a second version of the same fact.
//! * `launch` fails with `unsupported` when this host cannot present the kind —
//!   a host without a browser view refuses a `web` surface rather than opening
//!   something it cannot draw.

use std::fmt;
use std::path::{Path, PathBuf};

use soil_spore::{LaunchError, Manifest};

/// How a host presents a surface. Native is the default and has no variant, so
/// it is not a choice a package or a host makes here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SurfaceKind {
    Canvas,
    Web,
}

impl SurfaceKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Web => "web",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "canvas" => Some(Self::Canvas),
            "web" => Some(Self::Web),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    pub name: String,
    pub kind: SurfaceKind,
    /// Entry point relative to the package root, already resolved.
    pub entry: String,
    pub requires: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    /// Registry slug: identity, not a label.
    pub name: String,
    /// What a window title or a launcher row shows.
    pub title: String,
    pub version: String,
    pub summary: String,
    /// Supplied by host policy; this host does not invent one.
    pub workspace: Option<String>,
    /// The surface a bare launch opens.
    pub default_surface: String,
    pub surfaces: Vec<Surface>,
    /// The digest the host installed, when the install recorded one.
    pub digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub id: String,
    pub surface: String,
    /// What the host hands its renderer: the artifact inside the package for a
    /// page, the canvas module for a composed view.
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Host policy refused the request.
    Denied(String),
    NotFound(String),
    Invalid(String),
    /// This host cannot present the surface's kind.
    Unsupported(String),
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied(message) => write!(formatter, "denied: {message}"),
            Self::NotFound(message) => write!(formatter, "not found: {message}"),
            Self::Invalid(message) => write!(formatter, "invalid: {message}"),
            Self::Unsupported(message) => write!(formatter, "unsupported: {message}"),
            Self::Io(message) => write!(formatter, "io: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Every installed Spore under `root`, in id order.
///
/// A directory with no manifest is not a Spore and is skipped; a manifest that
/// does not parse is also skipped, because one broken tree must not hide every
/// other Spore on the machine.
pub fn entries(root: &Path) -> Result<Vec<Entry>> {
    let recorded = digests(root);
    let mut found = Vec::new();
    for dir in trees(root)? {
        let Ok(manifest) = Manifest::load(&dir) else {
            continue;
        };
        found.push(entry(&manifest, recorded.get(&manifest.spore.id).cloned()));
    }
    found.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(found)
}

/// Resolve `surface` of the installed Spore `id` against what this host can
/// present. This decides; it does not draw.
pub fn launch(root: &Path, id: &str, surface: &str, host: &[SurfaceKind]) -> Result<Launched> {
    let dir = trees(root)?
        .into_iter()
        .find(|dir| {
            Manifest::load(dir)
                .map(|manifest| manifest.spore.id == id)
                .unwrap_or(false)
        })
        .ok_or_else(|| Error::NotFound(format!("no installed Spore {id}")))?;
    let manifest = Manifest::load(&dir)
        .map_err(|error| Error::Invalid(format!("{}: {error}", dir.display())))?;

    let kinds: Vec<soil_spore::SurfaceKind> = host
        .iter()
        .map(|kind| match kind {
            SurfaceKind::Canvas => soil_spore::SurfaceKind::Canvas,
            SurfaceKind::Web => soil_spore::SurfaceKind::Web,
        })
        .collect();
    let target = manifest
        .resolve_launch(surface, &kinds)
        .map_err(|error| match error {
            LaunchError::NotFound { .. } => {
                Error::NotFound(format!("{id} declares no surface {surface:?}"))
            }
            LaunchError::Unsupported {
                kind,
                surface: name,
                ..
            } => Error::Unsupported(format!(
                "{id} surface {name:?} is {}, which this host cannot present",
                kind.as_str()
            )),
        })?;

    // A composed view opens its canvas module; a page opens its artifact. Both
    // are paths or names inside the package, never a second identity.
    let target = match target.entry {
        Some(entry) if target.kind == Some(soil_spore::SurfaceKind::Web) => entry.to_owned(),
        _ => manifest
            .launch
            .canvas_module
            .clone()
            .unwrap_or_else(|| format!("spore-{}", id.replace('.', "-"))),
    };
    Ok(Launched {
        id: id.to_owned(),
        surface: target_surface_name(&manifest, surface),
        target,
    })
}

/// The name the launch answers with: a declared surface's own name, or the
/// requested kind when the package declares no surfaces.
fn target_surface_name(manifest: &Manifest, requested: &str) -> String {
    manifest
        .surface(requested)
        .map(|surface| surface.name.clone())
        .unwrap_or_else(|| requested.to_owned())
}

/// One manifest, as the interface reports it.
fn entry(manifest: &Manifest, digest: Option<String>) -> Entry {
    let surfaces = if manifest.surfaces.is_empty() {
        // No declared surfaces: the kinds in `[launch] surfaces` are the names,
        // which is the behaviour every manifest had before surfaces existed.
        manifest
            .launch
            .surfaces
            .iter()
            .filter_map(|kind| SurfaceKind::parse(kind))
            .map(|kind| Surface {
                name: kind.as_str().to_owned(),
                kind,
                entry: conventional_entry(manifest, kind),
                requires: None,
            })
            .collect()
    } else {
        manifest
            .surfaces
            .iter()
            .filter_map(|surface| {
                let kind = SurfaceKind::parse(surface.kind.as_str())?;
                Some(Surface {
                    name: surface.name.clone(),
                    kind,
                    entry: surface
                        .entry(manifest.ui.as_ref())
                        .unwrap_or_default()
                        .to_owned(),
                    requires: surface.requires.clone(),
                })
            })
            .collect()
    };
    Entry {
        id: manifest.spore.id.clone(),
        name: manifest.spore.name.clone(),
        title: manifest.spore.name.clone(),
        version: manifest.spore.version.clone(),
        summary: manifest.spore.summary.clone(),
        workspace: None,
        default_surface: manifest.launch.default.clone(),
        surfaces,
        digest,
    }
}

/// `web/index.html`, or the package's own Lua entry.
fn conventional_entry(manifest: &Manifest, kind: SurfaceKind) -> String {
    match kind {
        SurfaceKind::Web => "web/index.html".to_owned(),
        SurfaceKind::Canvas => manifest
            .ui
            .as_ref()
            .map(|ui| ui.entry.clone())
            .unwrap_or_default(),
    }
}

/// The digest each installed Spore recorded at install time, when it recorded
/// one. A tree copied in by hand has no provenance, and the interface says so
/// with `None` rather than guessing from the bytes.
fn digests(root: &Path) -> std::collections::BTreeMap<String, String> {
    soil_install::status(root)
        .unwrap_or_default()
        .into_iter()
        .map(|state| (state.id, state.digest))
        .collect()
}

/// Installed trees: a directory under the root with a manifest in it.
fn trees(root: &Path) -> Result<Vec<PathBuf>> {
    let read = std::fs::read_dir(root)
        .map_err(|error| Error::Io(format!("{}: {error}", root.display())))?;
    let mut found = Vec::new();
    for entry in read.flatten() {
        let dir = entry.path();
        if dir.join("spore.toml").is_file() {
            found.push(dir);
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path, id: &str, extra: &str) {
        let dir = root.join(id.rsplit('.').next().unwrap_or(id));
        std::fs::create_dir_all(dir.join("ui")).expect("ui");
        std::fs::write(
            dir.join("spore.toml"),
            format!(
                "schema = 1\n[spore]\nid = \"{id}\"\nname = \"{}\"\nversion = \"0.1.0\"\n\
                 license = \"MIT\"\nsummary = \"a test Spore\"\n[engine]\nsoil = \">=0.1\"\n\
                 target = \"wasm32-wasip2\"\n[ui]\nentry = \"ui/main.lua\"\n{extra}",
                id.rsplit('.').next().unwrap_or(id)
            ),
        )
        .expect("manifest");
        std::fs::write(dir.join("ui/main.lua"), "return {}").expect("view");
    }

    #[test]
    fn a_hand_copied_tree_is_listed_without_provenance() {
        let root = tempfile::tempdir().expect("tempdir");
        tree(
            root.path(),
            "dev.fungos.canvas-one",
            "[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n",
        );
        tree(
            root.path(),
            "dev.fungos.page-one",
            "[launch]\ndefault = \"store\"\nsurfaces = [\"web\"]\n\
             [[surfaces]]\nname = \"store\"\nkind = \"web\"\n",
        );

        let listed = entries(root.path()).expect("entries");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "dev.fungos.canvas-one");
        assert_eq!(listed[0].digest, None, "a copied tree recorded nothing");
        assert_eq!(listed[0].default_surface, "canvas");
        assert_eq!(listed[0].surfaces[0].entry, "ui/main.lua");

        let page = &listed[1];
        assert_eq!(page.default_surface, "store");
        assert_eq!(page.surfaces[0].kind, SurfaceKind::Web);
        assert_eq!(page.surfaces[0].entry, "web/index.html");
    }

    #[test]
    fn a_host_refuses_a_surface_it_cannot_present() {
        let root = tempfile::tempdir().expect("tempdir");
        tree(
            root.path(),
            "dev.fungos.page-one",
            "[launch]\ndefault = \"store\"\nsurfaces = [\"web\"]\n\
             [[surfaces]]\nname = \"store\"\nkind = \"web\"\n",
        );
        let launched = launch(
            root.path(),
            "dev.fungos.page-one",
            "store",
            &[SurfaceKind::Canvas, SurfaceKind::Web],
        )
        .expect("a browser host can present a page");
        assert_eq!(launched.surface, "store");
        assert_eq!(launched.target, "web/index.html");

        let refused = launch(
            root.path(),
            "dev.fungos.page-one",
            "store",
            &[SurfaceKind::Canvas],
        )
        .expect_err("no browser view");
        assert!(matches!(refused, Error::Unsupported(_)), "{refused}");

        let missing = launch(root.path(), "dev.fungos.nope", "store", &[SurfaceKind::Web])
            .expect_err("not installed");
        assert!(matches!(missing, Error::NotFound(_)), "{missing}");
    }
}
