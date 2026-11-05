//! Installed Spore catalog and host-owned launch boundary.
//!
//! The host owns package discovery, validation and target selection. A guest
//! receives only the bounded catalog and can request one declared surface.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use wasmtime::component::{HasSelf, Linker};

wasmtime::component::bindgen!({ path: "wit", world: "spore-host" });

use soil::spore::catalog;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SporeEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub summary: String,
    pub workspace: Option<String>,
    pub default_surface: String,
    pub surfaces: Vec<String>,
    pub root: PathBuf,
}

pub trait Launcher {
    /// Launch one validated Spore surface and return the host target name.
    fn launch(&mut self, entry: &SporeEntry, surface: &str) -> Result<String, String>;
}

pub struct SporeCatalog {
    root: PathBuf,
    entries: Vec<SporeEntry>,
}

impl SporeCatalog {
    pub fn scan(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&root)
            .with_context(|| format!("read Spore root {}", root.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let package = entry.path();
            if !package.join("spore.toml").is_file() {
                continue;
            }
            let manifest = soil_spore::Manifest::load(&package)
                .with_context(|| format!("load Spore {}", package.display()))?;
            entries.push(SporeEntry {
                id: manifest.spore.id.clone(),
                name: manifest.spore.name.clone(),
                version: manifest.spore.version.clone(),
                summary: manifest.spore.summary.clone(),
                workspace: Some(format!("{}-dev", manifest.spore.name)),
                default_surface: manifest.launch.default.clone(),
                surfaces: manifest.launch.surfaces.clone(),
                root: package,
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        Ok(Self { root, entries })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entries(&self) -> &[SporeEntry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&SporeEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

pub struct CatalogHost<L> {
    catalog: SporeCatalog,
    launcher: L,
}

impl<L: Launcher> CatalogHost<L> {
    pub fn new(catalog: SporeCatalog, launcher: L) -> Self {
        Self { catalog, launcher }
    }
}

impl<L: Launcher> catalog::Host for CatalogHost<L> {
    fn entries(&mut self) -> Result<Vec<catalog::Entry>, catalog::Error> {
        Ok(self
            .catalog
            .entries()
            .iter()
            .map(|entry| catalog::Entry {
                id: entry.id.clone(),
                name: entry.name.clone(),
                version: entry.version.clone(),
                summary: entry.summary.clone(),
                workspace: entry.workspace.clone(),
                default_surface: entry.default_surface.clone(),
                surfaces: entry.surfaces.clone(),
            })
            .collect())
    }

    fn launch(&mut self, id: String, surface: String) -> Result<catalog::Launched, catalog::Error> {
        let entry = self
            .catalog
            .get(&id)
            .cloned()
            .ok_or(catalog::Error::NotFound)?;
        if !entry.surfaces.iter().any(|candidate| candidate == &surface) {
            return Err(catalog::Error::Invalid);
        }
        let target = self
            .launcher
            .launch(&entry, &surface)
            .map_err(|_| catalog::Error::Unsupported)?;
        Ok(catalog::Launched {
            id,
            surface,
            target,
        })
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> Result<()>
where
    T: catalog::Host + Send + 'static,
{
    catalog::add_to_linker::<T, HasSelf<T>>(linker, |state| state)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soil::spore::catalog::Host;

    #[derive(Default)]
    struct FakeLauncher {
        calls: usize,
    }

    impl Launcher for FakeLauncher {
        fn launch(&mut self, entry: &SporeEntry, surface: &str) -> Result<String, String> {
            self.calls += 1;
            Ok(format!("{}:{}", entry.id, surface))
        }
    }

    fn package(root: &Path, name: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::write(
            dir.join("spore.toml"),
            format!(
                r#"schema = 1
[spore]
id = "dev.fungos.{name}"
name = "{name}"
version = "0.1.0"
license = "MIT"
summary = "test"
[engine]
soil = ">=0.1"
target = "wasm32-wasip2"
[module]
path = "module.wasm"
[ui]
entry = "ui/app.lua"
[launch]
default = "canvas"
surfaces = ["canvas", "web"]
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn catalog_scans_and_launch_validates_surface() {
        let dir = tempfile::tempdir().unwrap();
        package(dir.path(), "alpha");
        package(dir.path(), "beta");
        let catalog = SporeCatalog::scan(dir.path()).unwrap();
        assert_eq!(catalog.entries().len(), 2);
        assert_eq!(catalog.entries()[0].workspace.as_deref(), Some("alpha-dev"));
        let mut host = CatalogHost::new(catalog, FakeLauncher::default());
        let entry = host.entries().unwrap()[0].clone();
        let launched = host.launch(entry.id.clone(), "canvas".into()).unwrap();
        assert_eq!(launched.target, format!("{}:canvas", entry.id));
        assert_eq!(
            host.launch(entry.id, "native".into()).unwrap_err(),
            catalog::Error::Invalid
        );
    }
}
