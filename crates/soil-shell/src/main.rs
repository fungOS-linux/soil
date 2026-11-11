//! A small shell front door for a fungOS VFS workspace.
//!
//! `soil shell` mounts the VFS through the existing FUSE adapter and starts an
//! interactive shell inside it. Writes are new content versions; `soil rm`
//! unlinks the namespace entry while retaining the content-addressed blob and
//! its history.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use fungos_vfs::Vfs;

mod spore;

fn main() {
    if let Err(error) = run() {
        eprintln!("soil: {error:#}");
        std::process::exit(1);
    }
}

fn usage() -> &'static str {
    "usage: soil --root ROOT <command>\n\
     commands:\n\
       spore run <dir|package.spore> [--listen ADDR] [--hub ADDR]... [--unibus-config FILE]\n\
                                    [--bridge ADDR] [--bridge-wait SECS] [--allow-origin URL]...\n\
       shell [--mount DIR] [--fuse-binary PATH] [--shell PATH] [--no-mount] [-- command...]\n\
       ls [PREFIX]\n\
       stat PATH\n\
       cat PATH\n\
       history PATH\n\
       rm PATH...\n\
       search QUERY"
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let root =
        take_option(&mut args, "--root").unwrap_or_else(|| "~/.local/share/fungos/vfs".into());
    let root = expand_home(&root);
    let Some(command) = args.first().cloned() else {
        bail!("{}", usage());
    };
    args.remove(0);
    match command.as_str() {
        // Any `.spore` on the names it claims: the same launcher for every
        // Spore, so a product never ships its own host again.
        "spore" => spore::run(&args),
        "shell" => shell(&root, args),
        "rm" => remove(&root, args),
        "ls" => list(&root, args),
        "stat" => stat(&root, args),
        "cat" => cat(&root, args),
        "history" => history(&root, args),
        "search" => search(&root, args),
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        other => bail!("unknown soil command {other}\n{}", usage()),
    }
}

fn shell(root: &Path, mut args: Vec<String>) -> Result<()> {
    let mount = take_option(&mut args, "--mount")
        .map(|value| expand_home(&value))
        .unwrap_or_else(|| root.join("..").join("fungOS"));
    let fuse = take_option(&mut args, "--fuse-binary").unwrap_or_else(|| "fungos-vfs-fuse".into());
    let shell = take_option(&mut args, "--shell")
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/sh".into());
    let no_mount = args.iter().any(|arg| arg == "--no-mount");
    args.retain(|arg| arg != "--no-mount");
    let command = args
        .iter()
        .position(|arg| arg == "--")
        .map(|position| args.split_off(position + 1));
    std::fs::create_dir_all(&mount).with_context(|| format!("create {}", mount.display()))?;

    let mounted_here = !no_mount && !is_mounted(&mount)?;
    let mut fuse_child = if mounted_here {
        Some(spawn_fuse(root, &mount, &fuse)?)
    } else {
        None
    };
    if let Some(child) = fuse_child.as_mut() {
        wait_for_mount(child, &mount)?;
    }

    let rc = write_safe_rc(root)?;
    let mut process = Command::new(&shell);
    process.current_dir(&mount);
    process.env("SOIL_VFS_ROOT", root);
    process.env("SOIL_MOUNT", &mount);
    process.env("SOIL_SAFE_RM", "1");
    if shell.ends_with("bash") {
        process.arg("--rcfile").arg(&rc).arg("-i");
    } else if shell.ends_with("zsh") {
        process.env("ZDOTDIR", rc.parent().unwrap_or(&rc));
    }
    if let Some(command) = command {
        process.arg("-c").arg(command.join(" "));
    }
    let status = process.status().context("start shell")?;
    let _ = std::fs::remove_file(&rc);
    if let Some(mut child) = fuse_child {
        let _ = child.kill();
        let _ = child.wait();
    }
    if !status.success() {
        bail!("shell exited with {status}");
    }
    Ok(())
}

fn remove(root: &Path, args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        bail!("usage: soil rm PATH...");
    }
    let mut vfs = Vfs::open(root).with_context(|| format!("open {}", root.display()))?;
    for path in args {
        let entry = vfs
            .file(&path)?
            .with_context(|| format!("not found: {path}"))?;
        vfs.remove(&path)?;
        println!(
            "unlinked {path}\n  content-id {}\n  version {}\n  blob retained; history: soil history {path}",
            entry.content_id, entry.version
        );
    }
    Ok(())
}

fn list(root: &Path, args: Vec<String>) -> Result<()> {
    let vfs = Vfs::open(root)?;
    let prefix = args.first().map(String::as_str).unwrap_or("");
    for entry in vfs.list(prefix)? {
        println!(
            "{}\t{}\t{}\t{}",
            entry.path, entry.size, entry.version, entry.content_id
        );
    }
    Ok(())
}

fn stat(root: &Path, args: Vec<String>) -> Result<()> {
    let Some(path) = args.first() else {
        bail!("usage: soil stat PATH");
    };
    let vfs = Vfs::open(root)?;
    let entry = vfs
        .file(path)?
        .with_context(|| format!("not found: {path}"))?;
    println!("{}", serde_json::to_string_pretty(&entry)?);
    Ok(())
}

fn cat(root: &Path, args: Vec<String>) -> Result<()> {
    let Some(path) = args.first() else {
        bail!("usage: soil cat PATH");
    };
    let mut vfs = Vfs::open(root)?;
    let bytes = vfs.get(path)?;
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}

fn history(root: &Path, args: Vec<String>) -> Result<()> {
    let Some(path) = args.first() else {
        bail!("usage: soil history PATH");
    };
    let vfs = Vfs::open(root)?;
    for entry in vfs.history(path)? {
        println!(
            "{}\t{}\t{}\t{}",
            entry.version, entry.size, entry.created_at_ms, entry.content_id
        );
    }
    Ok(())
}

fn search(root: &Path, args: Vec<String>) -> Result<()> {
    let query = args.join(" ");
    if query.trim().is_empty() {
        bail!("usage: soil search QUERY");
    }
    let mut host = soil_host::vfs_host::VfsHost::open(root)?;
    use soil_host::vfs_host::index::Host as _;
    let page = host.search(soil_host::vfs_host::index::Query {
        text: query,
        mode: Some(soil_host::vfs_host::index::Mode::Fuzzy),
        parent: None,
        media_type_prefix: None,
        offset: 0,
        limit: 50,
    })?;
    for hit in page.hits {
        println!(
            "{}\t{}\t{}",
            hit.path,
            hit.score,
            hit.snippet.unwrap_or_default()
        );
    }
    Ok(())
}

fn spawn_fuse(root: &Path, mount: &Path, binary: &str) -> Result<Child> {
    Command::new(binary)
        .arg("--root")
        .arg(root)
        .arg("--mount")
        .arg(mount)
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("start {binary}"))
}

fn wait_for_mount(child: &mut Child, mount: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            bail!("FUSE mount exited early: {status}");
        }
        if is_mounted(mount)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!("timed out mounting {}", mount.display())
}

fn is_mounted(path: &Path) -> Result<bool> {
    let output = Command::new("mount").output().context("run mount")?;
    let needle = format!(" on {} ", path.display());
    Ok(String::from_utf8_lossy(&output.stdout).contains(&needle))
}

fn write_safe_rc(root: &Path) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("soil-shell-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let rc = dir.join(".zshrc");
    std::fs::write(
        &rc,
        format!(
            "export SOIL_VFS_ROOT={root:?}\nSOIL_BIN={:?}\nalias rm=\"$SOIL_BIN rm --root \\\"$SOIL_VFS_ROOT\\\"\"\n",
            std::env::current_exe()?
        ),
    )?;
    Ok(rc)
}

fn take_option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == name)?;
    if index + 1 >= args.len() {
        return None;
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Some(value)
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest);
    }
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store inside the root's own state directory: it dies with the tempdir,
    /// and no other test (or the operator's real store) can see it.
    fn open_owned(root: &std::path::Path) -> Vfs {
        Vfs::open_with(
            root,
            fungos_vfs::NodeRole::Full,
            Some(&root.join(".fungos/store")),
        )
        .unwrap()
    }

    #[test]
    fn rm_unlinks_namespace_and_keeps_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut vfs = open_owned(dir.path());
        let entry = vfs
            .put(
                "notes.txt",
                b"keep this blob",
                "text/plain",
                fungos_vfs::Expect::Any,
            )
            .unwrap();
        remove(dir.path(), vec!["notes.txt".into()]).unwrap();
        assert!(vfs.file("notes.txt").unwrap().is_none());
        let history = vfs.history("notes.txt").unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content_id, entry.content_id);
    }
}
