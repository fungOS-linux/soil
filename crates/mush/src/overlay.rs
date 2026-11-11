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

/// What a file is when the VFS did not say: readable, not executable.
/// Matches the default `fungos-vfs` gives a path nobody has written.
const NEW_FILE_MODE: u32 = 0o644;

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
    /// Permission bits the VFS carried. A build output that is a program must
    /// come back out of the namespace as one, and a `chmod` in the overlay has
    /// to be visible as a change even when the bytes are identical.
    mode: u32,
}

/// One file in the overlay as it stands now.
struct CurrentFile {
    bytes: Vec<u8>,
    mode: u32,
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

/// A scratch directory for one overlay.
///
/// `TMPDIR` is inherited, and an inherited `TMPDIR` can name a directory that no
/// longer exists: an agent session's per-turn scratch directory is deleted
/// between turns, and `tempfile` then fails with `ENOENT` — which would make
/// every host command fail for the rest of the session. Fall back to the system
/// temp instead of making that the caller's problem.
fn overlay_tempdir() -> Result<TempDir> {
    #[cfg(unix)]
    const FALLBACK: &str = "/tmp";
    #[cfg(not(unix))]
    const FALLBACK: &str = "C:\\Windows\\Temp";
    match tempfile::tempdir() {
        Ok(dir) => Ok(dir),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tempfile::tempdir_in(FALLBACK).context("create overlay directory")
        }
        Err(err) => Err(err).context("create overlay directory"),
    }
}

impl Overlay {
    pub fn materialize(vfs_root: impl AsRef<Path>) -> Result<Self> {
        let dir = overlay_tempdir()?;
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
            set_file_mode(&target, entry.mode)?;
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

    fn changes(&self, current: &BTreeMap<String, CurrentFile>) -> Vec<Change> {
        let mut changes = Vec::new();
        for (path, file) in current {
            match self.original.get(path) {
                None => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Added,
                }),
                Some(snapshot) if &snapshot.bytes != &file.bytes || snapshot.mode != file.mode => {
                    changes.push(Change {
                        path: path.clone(),
                        kind: ChangeKind::Modified,
                    })
                }
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
                    let file = current
                        .get(&change.path)
                        .with_context(|| format!("overlay lost {}", change.path))?;
                    let media = media_type(&change.path);
                    // Only the mode moved: write the entry, not a new version of
                    // bytes nobody changed.
                    let mode_only = self
                        .original
                        .get(&change.path)
                        .is_some_and(|snapshot| snapshot.bytes == file.bytes);
                    if mode_only {
                        vfs.set_mode(&change.path, file.mode)?;
                    } else {
                        vfs.put_with_mode(&change.path, &file.bytes, media, file.mode, Expect::Any)?;
                    }
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

    fn current_files(&self) -> Result<BTreeMap<String, CurrentFile>> {
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
            let metadata = entry.metadata().ok();
            let stamp = metadata.as_ref().map(FileStamp::of);
            let bytes = match self.original.get(&relative) {
                // Same size and mtime: these are the bytes we materialized, so
                // there is nothing to read.
                Some(snapshot) if stamp == Some(snapshot.stamp) => snapshot.bytes.clone(),
                _ => fs::read(entry.path())?,
            };
            let mode = metadata.as_ref().map(file_mode).unwrap_or(NEW_FILE_MODE);
            out.insert(relative, CurrentFile { bytes, mode });
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
        let mode = entry
            .metadata()
            .as_ref()
            .map(file_mode)
            .unwrap_or(NEW_FILE_MODE);
        files.insert(
            relative,
            Snapshot {
                bytes,
                stamp,
                mode,
            },
        );
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

/// Permission bits of a materialized file.
#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    NEW_FILE_MODE
}

/// Give a materialized file the mode the namespace remembers.
///
/// A build that produces an executable writes it through this path; without it
/// the VFS hands back a file nobody can run.
#[cfg(unix)]
fn set_file_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set mode on {}", path.display()))
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
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
