//! Host-side VFS overlay for an agent session.
//!
//! The overlay is a materialized worktree plus a snapshot of the VFS files it
//! came from. An agent can mutate the worktree freely. On exit, the host shows
//! a diff and applies selected changes back as new VFS versions.
//!
//! `.gitignore`, `.ignore`, and `.soilignore` are respected in both directions:
//! ignored build outputs never enter the diff and never become VFS content.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use fungos_vfs::{Expect, NodeRole, Vfs};
use ignore::WalkBuilder;
use tempfile::TempDir;

const IGNORED: &[&str] = &[
    ".operator/",
    ".cache/",
    ".worktrees/",
    "build/",
    "dist/",
    "node_modules/",
    "target/",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug)]
pub struct ApplyReport {
    pub written: usize,
    pub deleted: usize,
    pub directories: usize,
}

pub struct Overlay {
    dir: TempDir,
    original: BTreeMap<String, Vec<u8>>,
    original_dirs: BTreeSet<String>,
}

impl Overlay {
    pub fn materialize(vfs_root: impl AsRef<Path>, store: Option<&Path>) -> Result<Self> {
        let dir = tempfile::tempdir().context("create overlay directory")?;
        let mut vfs =
            Vfs::open_with(vfs_root, NodeRole::Full, store).context("open VFS for overlay")?;
        for entry in vfs.list("")? {
            let bytes = vfs.get(&entry.path)?;
            let target = safe_join(dir.path(), &entry.path)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, &bytes).with_context(|| format!("write overlay {}", entry.path))?;
        }
        for path in vfs.directories("")? {
            fs::create_dir_all(safe_join(dir.path(), &path)?)?;
        }

        // Snapshot after materialization so nested ignore files apply to the
        // exact overlay tree the agent will see.
        let original = collect_files(dir.path())?;
        let original_dirs = collect_dirs(dir.path())?;
        Ok(Self {
            dir,
            original,
            original_dirs,
        })
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn keep(self) -> PathBuf {
        self.dir.keep()
    }

    pub fn diff(&self) -> Result<Vec<Change>> {
        let current = self.current_files()?;
        let mut changes = Vec::new();
        for (path, bytes) in &current {
            match self.original.get(path) {
                None => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Added,
                }),
                Some(original) if original != bytes => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Modified,
                }),
                Some(_) => {}
            }
        }
        for path in self.original.keys() {
            if !current.contains_key(path) {
                changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Deleted,
                });
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(changes)
    }

    pub fn apply(&self, vfs_root: impl AsRef<Path>, store: Option<&Path>) -> Result<ApplyReport> {
        let mut vfs =
            Vfs::open_with(vfs_root, NodeRole::Full, store).context("open VFS to apply overlay")?;
        let changes = self.diff()?;
        let current = self.current_files()?;
        let mut written = 0;
        let mut deleted = 0;
        let mut directories = 0;

        for change in &changes {
            match change.kind {
                ChangeKind::Added | ChangeKind::Modified => {
                    let bytes = current
                        .get(&change.path)
                        .with_context(|| format!("overlay lost {}", change.path))?;
                    let media = media_type(&change.path);
                    vfs.put(&change.path, bytes, media, Expect::Any)?;
                    written += 1;
                }
                ChangeKind::Deleted => {
                    if vfs.file(&change.path)?.is_some() {
                        vfs.remove(&change.path)?;
                        deleted += 1;
                    }
                }
            }
        }

        for dir in self.current_dirs()? {
            if vfs.is_directory(&dir)? {
                continue;
            }
            if let Some(parent) = Path::new(&dir).parent() {
                let parent = parent.to_string_lossy().replace('\\', "/");
                if !parent.is_empty() && !vfs.is_directory(&parent)? {
                    continue;
                }
            }
            vfs.mkdir(&dir)?;
            directories += 1;
        }

        for dir in &self.original_dirs {
            if !current_dir_exists(self.path(), dir) && vfs.is_directory(dir)? {
                let _ = vfs.rmdir(dir);
            }
        }

        Ok(ApplyReport {
            written,
            deleted,
            directories,
        })
    }

    fn current_files(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        collect_files(self.path())
    }

    fn current_dirs(&self) -> Result<Vec<String>> {
        let mut dirs: Vec<String> = collect_dirs(self.path())?.into_iter().collect();
        dirs.sort();
        Ok(dirs)
    }
}

fn walker(root: &Path) -> Result<ignore::Walk> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(true)
        .ignore(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .add_custom_ignore_filename(".soilignore");
    Ok(builder.build())
}

fn collect_files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    for entry in walker(root)? {
        let entry = entry?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let relative = relative(root, entry.path())?;
        if ignored(&relative) {
            continue;
        }
        out.insert(relative, fs::read(entry.path())?);
    }
    Ok(out)
}

fn collect_dirs(root: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for entry in walker(root)? {
        let entry = entry?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let relative = relative(root, entry.path())?;
        if relative.is_empty() || ignored(&relative) {
            continue;
        }
        out.insert(relative);
    }
    Ok(out)
}

fn relative(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| format!("strip {}", path.display()))?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn current_dir_exists(root: &Path, relative: &str) -> bool {
    safe_join(root, relative)
        .map(|path| path.is_dir())
        .unwrap_or(false)
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative
        .split('/')
        .any(|part| part == ".." || part.contains('\\'))
    {
        anyhow::bail!("unsafe overlay path {relative}");
    }
    Ok(root.join(relative))
}

pub fn ignored(path: &str) -> bool {
    IGNORED
        .iter()
        .any(|prefix| path == prefix.trim_end_matches('/') || path.starts_with(prefix))
}

fn media_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("rs") => "text/x-rust",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some("toml") => "application/toml",
        Some("yaml" | "yml") => "application/yaml",
        Some("sh" | "bash") => "text/x-shellscript",
        _ => "text/plain",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store this test owns. Inside `.fungos`, so it is the VFS's own state
    /// directory rather than workspace content, and it dies with the tempdir.
    fn owned(root: &Path) -> std::path::PathBuf {
        root.join(".fungos/store")
    }

    #[test]
    fn overlay_applies_add_modify_delete_and_empty_dirs() {
        let vfs = tempfile::tempdir().unwrap();
        {
            let mut store =
                Vfs::open_with(vfs.path(), NodeRole::Full, Some(&owned(vfs.path()))).unwrap();
            store
                .put("notes/a.txt", b"old", "text/plain", Expect::Any)
                .unwrap();
            store
                .put("notes/b.txt", b"remove me", "text/plain", Expect::Any)
                .unwrap();
            store.mkdir("empty").unwrap();
        }

        let overlay = Overlay::materialize(vfs.path(), Some(&owned(vfs.path()))).unwrap();
        fs::write(overlay.path().join("notes/a.txt"), b"new").unwrap();
        fs::remove_file(overlay.path().join("notes/b.txt")).unwrap();
        fs::write(overlay.path().join("new.txt"), b"fresh").unwrap();
        fs::create_dir_all(overlay.path().join("empty/nested")).unwrap();

        let report = overlay.apply(vfs.path(), Some(&owned(vfs.path()))).unwrap();
        assert_eq!(report.written, 2);
        assert_eq!(report.deleted, 1);
        assert!(report.directories >= 1);
        let mut store =
            Vfs::open_with(vfs.path(), NodeRole::Full, Some(&owned(vfs.path()))).unwrap();
        assert_eq!(store.get("notes/a.txt").unwrap(), b"new");
        assert!(store.file("notes/b.txt").unwrap().is_none());
        assert_eq!(store.get("new.txt").unwrap(), b"fresh");
        assert!(store.is_directory("empty/nested").unwrap());
    }

    #[test]
    fn overlay_tracks_git_metadata() {
        let vfs = tempfile::tempdir().unwrap();
        {
            let mut store =
                Vfs::open_with(vfs.path(), NodeRole::Full, Some(&owned(vfs.path()))).unwrap();
            store
                .put(".git/config", b"[core]\n", "text/plain", Expect::Any)
                .unwrap();
        }
        let overlay = Overlay::materialize(vfs.path(), Some(&owned(vfs.path()))).unwrap();
        assert!(overlay.path().join(".git/config").is_file());
        fs::write(
            overlay.path().join(".git/config"),
            b"[core]\n\tbranch = main\n",
        )
        .unwrap();
        let report = overlay.apply(vfs.path(), Some(&owned(vfs.path()))).unwrap();
        assert_eq!(report.written, 1);
        let mut store =
            Vfs::open_with(vfs.path(), NodeRole::Full, Some(&owned(vfs.path()))).unwrap();
        assert_eq!(
            store.get(".git/config").unwrap(),
            b"[core]\n\tbranch = main\n"
        );
    }

    #[test]
    fn overlay_respects_gitignore_for_build_outputs() {
        let vfs = tempfile::tempdir().unwrap();
        {
            let mut store =
                Vfs::open_with(vfs.path(), NodeRole::Full, Some(&owned(vfs.path()))).unwrap();
            store
                .put(".gitignore", b"target/\n*.bin\n", "text/plain", Expect::Any)
                .unwrap();
            store
                .put("src/lib.rs", b"old", "text/x-rust", Expect::Any)
                .unwrap();
            store
                .put(
                    "target/debug/app",
                    b"old binary",
                    "application/octet-stream",
                    Expect::Any,
                )
                .unwrap();
        }
        let overlay = Overlay::materialize(vfs.path(), Some(&owned(vfs.path()))).unwrap();
        fs::write(overlay.path().join("src/lib.rs"), b"new").unwrap();
        fs::create_dir_all(overlay.path().join("target/release")).unwrap();
        fs::write(overlay.path().join("target/release/app"), b"new binary").unwrap();
        fs::write(overlay.path().join("artifact.bin"), b"binary").unwrap();

        let changes = overlay.diff().unwrap();
        assert!(changes.iter().any(|change| change.path == "src/lib.rs"));
        assert!(!changes.iter().any(|change| change.path.ends_with(".bin")));
        assert!(
            !changes
                .iter()
                .any(|change| change.path.starts_with("target/"))
        );

        overlay.apply(vfs.path(), Some(&owned(vfs.path()))).unwrap();
        let mut store =
            Vfs::open_with(vfs.path(), NodeRole::Full, Some(&owned(vfs.path()))).unwrap();
        assert_eq!(store.get("src/lib.rs").unwrap(), b"new");
        assert!(store.file("artifact.bin").unwrap().is_none());
        assert_eq!(
            store.get("target/debug/app").unwrap(),
            b"old binary",
            "ignored files must not be deleted or rewritten"
        );
    }
}
