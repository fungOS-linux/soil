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
use fungos_vfs::{Expect, Vfs};
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
    original: BTreeMap<String, Snapshot>,
    original_dirs: BTreeSet<String>,
}

/// What a file looked like when the overlay was materialized.
///
/// The bytes are what confirms a change exactly; the size and mtime are what
/// let an unchanged file skip being read at all. A host command that changed
/// nothing — `git status`, a build writing only gitignored output — otherwise
/// reads the whole tree twice.
struct Snapshot {
    bytes: Vec<u8>,
    stamp: FileStamp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    size: u64,
    mtime: Option<std::time::SystemTime>,
}

impl FileStamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            size: metadata.len(),
            mtime: metadata.modified().ok(),
        }
    }
}

impl Overlay {
    pub fn materialize(vfs_root: impl AsRef<Path>) -> Result<Self> {
        let dir = tempfile::tempdir().context("create overlay directory")?;
        let mut vfs = Vfs::open(vfs_root).context("open VFS for overlay")?;
        let mut written: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for entry in vfs.list("")? {
            let bytes = vfs.get(&entry.path)?;
            let target = safe_join(dir.path(), &entry.path)?;
            if let Some(parent) = target.parent() {
                // A path in the VFS can name both a file and a directory (a
                // `directories` row that outlived the file replacing it, or a
                // file claiming a parent that is itself a file). No filesystem
                // can hold that, and one such entry must not fail every host
                // command for the workspace, so the entry that cannot be
                // represented is skipped and the rest still materializes.
                if parent.is_file() {
                    continue;
                }
                fs::create_dir_all(parent)
                    .with_context(|| format!("create overlay directory {}", parent.display()))?;
            }
            fs::write(&target, &bytes).with_context(|| format!("write overlay {}", entry.path))?;
            written.insert(entry.path.clone(), bytes);
        }
        for path in vfs.directories("")? {
            let target = safe_join(dir.path(), &path)?;
            // The file won above: `stat` in the VFS reports the file, and a
            // directory cannot also exist at that path.
            if target.is_file() {
                continue;
            }
            fs::create_dir_all(&target)
                .with_context(|| format!("create overlay directory {}", target.display()))?;
        }

        // Snapshot after materialization so nested ignore files apply to the
        // exact overlay tree the agent will see. The bytes come from the write
        // pass, not a second read of every file.
        let (original, original_dirs) = snapshot(dir.path(), &mut written)?;
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
        Ok(self.changes(&self.current_files()?))
    }

    fn changes(&self, current: &BTreeMap<String, Vec<u8>>) -> Vec<Change> {
        let mut changes = Vec::new();
        for (path, bytes) in current {
            match self.original.get(path) {
                None => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Added,
                }),
                Some(snapshot) if &snapshot.bytes != bytes => changes.push(Change {
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
        changes
    }

    pub fn apply(&self, vfs_root: impl AsRef<Path>) -> Result<ApplyReport> {
        let mut vfs = Vfs::open(vfs_root).context("open VFS to apply overlay")?;
        let current = self.current_files()?;
        let changes = self.changes(&current);
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
        let mut out = BTreeMap::new();
        for entry in walker(self.path())? {
            let entry = entry?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let relative = relative(self.path(), entry.path())?;
            if ignored(&relative) {
                continue;
            }
            let stamp = entry.metadata().ok().map(|metadata| FileStamp::of(&metadata));
            match self.original.get(&relative) {
                // Same size and mtime: these are the bytes we materialized, so
                // there is nothing to read.
                Some(snapshot) if stamp == Some(snapshot.stamp) => {
                    out.insert(relative, snapshot.bytes.clone());
                }
                _ => {
                    out.insert(relative, fs::read(entry.path())?);
                }
            }
        }
        Ok(out)
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

fn collect_dirs(root: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for entry in walker(root)? {
        let entry = entry?;
        if !entry.file_type().is_some_and(|kind| kind.is_dir()) {
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

/// Snapshot the materialized tree in one walk: the bytes that were written,
/// stamped with the size and mtime the filesystem gave them.
fn snapshot(
    root: &Path,
    written: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(BTreeMap<String, Snapshot>, BTreeSet<String>)> {
    let mut files = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    for entry in walker(root)? {
        let entry = entry?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        let relative = relative(root, entry.path())?;
        if ignored(&relative) {
            continue;
        }
        if file_type.is_dir() {
            if !relative.is_empty() {
                dirs.insert(relative);
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let bytes = match written.remove(&relative) {
            Some(bytes) => bytes,
            None => fs::read(entry.path())?,
        };
        let stamp = entry
            .metadata()
            .map(|metadata| FileStamp::of(&metadata))
            .unwrap_or(FileStamp {
                size: 0,
                mtime: None,
            });
        files.insert(relative, Snapshot { bytes, stamp });
    }
    Ok((files, dirs))
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

    #[test]
    fn overlay_applies_add_modify_delete_and_empty_dirs() {
        let vfs = tempfile::tempdir().unwrap();
        {
            let mut store = Vfs::open(vfs.path()).unwrap();
            store
                .put("notes/a.txt", b"old", "text/plain", Expect::Any)
                .unwrap();
            store
                .put("notes/b.txt", b"remove me", "text/plain", Expect::Any)
                .unwrap();
            store.mkdir("empty").unwrap();
        }

        let overlay = Overlay::materialize(vfs.path()).unwrap();
        fs::write(overlay.path().join("notes/a.txt"), b"new").unwrap();
        fs::remove_file(overlay.path().join("notes/b.txt")).unwrap();
        fs::write(overlay.path().join("new.txt"), b"fresh").unwrap();
        fs::create_dir_all(overlay.path().join("empty/nested")).unwrap();

        let report = overlay.apply(vfs.path()).unwrap();
        assert_eq!(report.written, 2);
        assert_eq!(report.deleted, 1);
        assert!(report.directories >= 1);
        let mut store = Vfs::open(vfs.path()).unwrap();
        assert_eq!(store.get("notes/a.txt").unwrap(), b"new");
        assert!(store.file("notes/b.txt").unwrap().is_none());
        assert_eq!(store.get("new.txt").unwrap(), b"fresh");
        assert!(store.is_directory("empty/nested").unwrap());
    }

    #[test]
    #[ignore = "timing benchmark; run with `--ignored --nocapture`"]
    fn bench_overlay_round_trip() {
        use std::time::Instant;

        // Shapes a real workspace: many small files, and a host command that
        // changed nothing (the common case: `git status`, a build that only
        // touches gitignored output).
        for files in [250usize, 2_000, 10_000] {
            let vfs = tempfile::tempdir().unwrap();
            let body = vec![b'x'; 2048];
            {
                let mut store = Vfs::open(vfs.path()).unwrap();
                for index in 0..files {
                    store
                        .put(
                            &format!("src/f{index:05}.rs"),
                            &body,
                            "text/x-rust",
                            Expect::Any,
                        )
                        .unwrap();
                }
            }
            let start = Instant::now();
            let overlay = Overlay::materialize(vfs.path()).unwrap();
            let materialize = start.elapsed();
            let start = Instant::now();
            let report = overlay.apply(vfs.path()).unwrap();
            let apply = start.elapsed();
            println!(
                "{files:>6} files x 2 KiB: materialize {:?}, apply {:?} ({} written)",
                materialize, apply, report.written
            );
        }
    }

    #[test]
    fn overlay_survives_a_path_that_is_both_a_file_and_a_directory() {
        let vfs = tempfile::tempdir().unwrap();
        {
            let mut store = Vfs::open(vfs.path()).unwrap();
            // A `directories` row can outlive the file that replaced it: `put`
            // only consults the file table, so both name `docs/a.txt` and no
            // filesystem can hold both.
            store.mkdir("docs").unwrap();
            store.mkdir("docs/a.txt").unwrap();
            store
                .put("docs/a.txt", b"file wins", "text/plain", Expect::Any)
                .unwrap();
            store
                .put("docs/ok.txt", b"ok", "text/plain", Expect::Any)
                .unwrap();
            store.mkdir("stale").unwrap();
        }

        let overlay = Overlay::materialize(vfs.path()).unwrap();
        assert!(overlay.path().join("docs/a.txt").is_file());
        assert_eq!(
            fs::read(overlay.path().join("docs/a.txt")).unwrap(),
            b"file wins"
        );
        assert!(overlay.path().join("docs/ok.txt").is_file());
        assert!(overlay.path().join("stale").is_dir());
    }

    #[test]
    fn overlay_tracks_git_metadata() {
        let vfs = tempfile::tempdir().unwrap();
        {
            let mut store = Vfs::open(vfs.path()).unwrap();
            store
                .put(".git/config", b"[core]\n", "text/plain", Expect::Any)
                .unwrap();
        }
        let overlay = Overlay::materialize(vfs.path()).unwrap();
        assert!(overlay.path().join(".git/config").is_file());
        fs::write(
            overlay.path().join(".git/config"),
            b"[core]\n\tbranch = main\n",
        )
        .unwrap();
        let report = overlay.apply(vfs.path()).unwrap();
        assert_eq!(report.written, 1);
        let mut store = Vfs::open(vfs.path()).unwrap();
        assert_eq!(
            store.get(".git/config").unwrap(),
            b"[core]\n\tbranch = main\n"
        );
    }

    #[test]
    fn overlay_respects_gitignore_for_build_outputs() {
        let vfs = tempfile::tempdir().unwrap();
        {
            let mut store = Vfs::open(vfs.path()).unwrap();
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
        let overlay = Overlay::materialize(vfs.path()).unwrap();
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

        overlay.apply(vfs.path()).unwrap();
        let mut store = Vfs::open(vfs.path()).unwrap();
        assert_eq!(store.get("src/lib.rs").unwrap(), b"new");
        assert!(store.file("artifact.bin").unwrap().is_none());
        assert_eq!(
            store.get("target/debug/app").unwrap(),
            b"old binary",
            "ignored files must not be deleted or rewritten"
        );
    }
}
