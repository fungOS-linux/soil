//! Project attach for a Desktop.
//!
//! This is a snapshot import, not a live worktree monitor. It copies a host
//! checkout into the Desktop VFS under `projects/<name>` so Mush can browse,
//! search, and edit it with VFS tools. A future worktree implementation will
//! replace this copy with Git base + op-log history.

use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use fungos_vfs::Vfs;
use ignore::WalkBuilder;
use serde_json::json;

use crate::{config, desktop};

const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 1024 * 1024 * 1024;

pub fn attach(desktop_id: &str, project: &str, source: &str) -> Result<(usize, usize)> {
    desktop::validate_id(project)?;
    let desktop_config = desktop::load(desktop_id)?;
    let source = config::expand_home(source);
    if !source.is_dir() {
        bail!("project source {} is not a directory", source.display());
    }
    let mut vfs = Vfs::open(&desktop_config.root)
        .with_context(|| format!("open Desktop VFS {}", desktop_config.root.display()))?;
    let head = git_output(&source, &["rev-parse", "HEAD"]);
    let dirty =
        git_output(&source, &["status", "--porcelain"]).map(|output| !output.trim().is_empty());
    let remote = git_output(&source, &["remote", "get-url", "origin"]);

    let mut files = 0usize;
    let mut imported_bytes = 0usize;
    let mut walker = WalkBuilder::new(&source);
    walker
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .parents(true);
    for entry in walker.build() {
        let entry = entry.map_err(|error| anyhow::anyhow!("walk {}: {error}", source.display()))?;
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if path.is_symlink() {
            continue;
        }
        let relative = match path.strip_prefix(&source) {
            Ok(relative) => relative,
            Err(_) => continue,
        };
        if relative.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some(".git" | "target" | "node_modules")
            )
        }) {
            continue;
        }
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        if bytes.len() > MAX_FILE_BYTES {
            continue;
        }
        if imported_bytes + bytes.len() > MAX_TOTAL_BYTES {
            bail!("project import exceeds {MAX_TOTAL_BYTES} bytes");
        }
        let vfs_path = format!(
            "projects/{project}/{}",
            relative.to_string_lossy().replace('\\', "/")
        );
        vfs.put(&vfs_path, &bytes, media_type(path), None)
            .with_context(|| format!("write VFS path {vfs_path}"))?;
        files += 1;
        imported_bytes += bytes.len();
    }
    if files == 0 {
        bail!("project {} produced no importable files", source.display());
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let manifest = json!({
        "schema": 1,
        "name": project,
        "source": source.display().to_string(),
        "head": head,
        "dirty": dirty.unwrap_or(false),
        "remote": remote,
        "files": files,
        "bytes": imported_bytes,
        "imported_at_ms": now_ms,
    });
    let manifest_path = format!("projects/{project}/.desktop/project.json");
    vfs.put(
        &manifest_path,
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
        "application/json",
        None,
    )
    .with_context(|| format!("write VFS path {manifest_path}"))?;
    desktop::add_project(desktop_id, project)?;
    Ok((files, imported_bytes))
}

fn git_output(source: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn media_type(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("rs") => "text/x-rust",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some("toml") => "application/toml",
        Some("yaml" | "yml") => "application/yaml",
        Some("sh") => "application/x-sh",
        Some("js") => "text/javascript",
        Some("ts") => "text/typescript",
        Some("py") => "text/python",
        Some("html") => "text/html",
        Some("css") => "text/css",
        Some("xml") => "text/xml",
        _ => "application/octet-stream",
    }
}
