//! What is actually inside a Desktop.
//!
//! An agent should ask the Desktop what it contains instead of being handed a
//! hand-written list in every session. This walks the declared project list and
//! the projection on disk, reports the difference (declared-but-missing,
//! present-but-undeclared), and adds enough repository detail to pick one.
//!
//! Repository detail comes from the host `git`, because the projection is a
//! real directory here. Nothing about this reads the VFS: it answers "what can
//! I work on", which is a question about the projection.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use serde_json::json;

use crate::desktop::{self, Desktop};

struct Git {
    branch: String,
    dirty: bool,
    remote: Option<String>,
}

struct Project {
    name: String,
    declared: bool,
    present: bool,
    git: Option<Git>,
    marbles: bool,
    path: Option<PathBuf>,
    github: Option<String>,
    gitea: Option<String>,
    archived: bool,
}

fn git(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_info(path: &Path) -> Option<Git> {
    if !path.join(".git").exists() {
        return None;
    }
    let branch = git(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let dirty = git(path, &["status", "--porcelain"])
        .map(|status| !status.is_empty())
        .unwrap_or(false);
    let remote = git(path, &["remote", "get-url", "origin"]);
    Some(Git {
        branch,
        dirty,
        remote,
    })
}

fn discover(desktop: &Desktop) -> Vec<Project> {
    let mut names: Vec<String> = desktop.projects.clone();
    let projects_dir = desktop.mount.join("projects");
    if let Ok(entries) = std::fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.sort();
    names.dedup();

    names
        .into_iter()
        .map(|name| {
            let path = projects_dir.join(&name);
            let present = path.is_dir();
            let declared = desktop.projects.iter().any(|project| project == &name);
            let git = present.then(|| git_info(&path)).flatten();
            let marbles =
                present && (path.join(".marbles").exists() || path.join(".marbles-local").exists());
            Project {
                name,
                declared,
                present,
                git,
                marbles,
                path: present.then_some(path),
                github: None,
                gitea: None,
                archived: false,
            }
        })
        .collect()
}

/// One repository as a remote host knows it, before deduping.
#[derive(Default, Clone)]
struct Remote {
    /// Lowercased repository name: the dedupe key across hosts.
    key: String,
    name: String,
    github: Option<String>,
    gitea: Option<String>,
    archived: bool,
    private: bool,
}

fn gh_repos() -> Result<Vec<Remote>> {
    let output = Command::new("gh")
        .args([
            "repo",
            "list",
            "--limit",
            "400",
            "--json",
            "name,nameWithOwner,isArchived,isPrivate",
        ])
        .output()
        .map_err(|error| anyhow::anyhow!("run gh repo list: {error}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "gh repo list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let name = row
                .get("name")
                .and_then(|value| value.as_str())?
                .to_string();
            let full = row
                .get("nameWithOwner")
                .and_then(|value| value.as_str())
                .unwrap_or(&name)
                .to_string();
            Some(Remote {
                key: name.to_lowercase(),
                name,
                github: Some(full),
                gitea: None,
                archived: row
                    .get("isArchived")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                private: row
                    .get("isPrivate")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect())
}

/// The Gitea mirror, when this machine is pointed at one.
///
/// Config: `GITEA_URL` and `GITEA_TOKEN` (a token with repo read scope). Absent
/// means "no mirror configured", which is not an error.
fn gitea_repos() -> Result<Vec<Remote>> {
    let (Ok(base), Ok(token)) = (std::env::var("GITEA_URL"), std::env::var("GITEA_TOKEN")) else {
        return Ok(Vec::new());
    };
    let user = std::env::var("GITEA_USER").unwrap_or_else(|_| "".into());
    let path = if user.is_empty() {
        format!("{}/api/v1/user/repos?limit=50", base.trim_end_matches('/'))
    } else {
        format!(
            "{}/api/v1/users/{}/repos?limit=50",
            base.trim_end_matches('/'),
            user
        )
    };
    let response = ureq::get(&path)
        .set("Authorization", &format!("token {token}"))
        .call()
        .map_err(|error| anyhow::anyhow!("gitea {path}: {error}"))?;
    let rows: Vec<serde_json::Value> = response.into_json()?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let name = row
                .get("name")
                .and_then(|value| value.as_str())?
                .to_string();
            let full = row
                .get("full_name")
                .and_then(|value| value.as_str())
                .unwrap_or(&name)
                .to_string();
            Some(Remote {
                key: name.to_lowercase(),
                name,
                github: None,
                gitea: Some(full),
                archived: row
                    .get("archived")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                private: row
                    .get("private")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect())
}

/// Merge hosts on the repository name, so a mirror is not a second project.
fn remotes() -> Result<BTreeMap<String, Remote>> {
    let mut merged: BTreeMap<String, Remote> = BTreeMap::new();
    let mut errors = Vec::new();
    for source in [gh_repos(), gitea_repos()] {
        match source {
            Ok(list) => {
                for remote in list {
                    merged
                        .entry(remote.key.clone())
                        .and_modify(|existing| {
                            existing.github = existing.github.take().or(remote.github.clone());
                            existing.gitea = existing.gitea.take().or(remote.gitea.clone());
                            existing.archived |= remote.archived;
                            existing.private |= remote.private;
                        })
                        .or_insert(remote);
                }
            }
            Err(error) => errors.push(error.to_string()),
        }
    }
    for error in errors {
        eprintln!("mush: {error}");
    }
    Ok(merged)
}

fn render(projects: &[Project], json_output: bool) {
    if json_output {
        let rows: Vec<_> = projects
            .iter()
            .map(|project| {
                json!({
                    "name": project.name,
                    "declared": project.declared,
                    "present": project.present,
                    "path": project.path.as_ref().map(|path| path.display().to_string()),
                    "branch": project.git.as_ref().map(|git| git.branch.clone()),
                    "dirty": project.git.as_ref().map(|git| git.dirty),
                    "remote": project.git.as_ref().and_then(|git| git.remote.clone()),
                    "marbles": project.marbles,
                    "github": project.github,
                    "gitea": project.gitea,
                    "archived": project.archived,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({ "projects": rows })).unwrap_or_default()
        );
        return;
    }
    if projects.is_empty() {
        println!("no projects in this Desktop");
        return;
    }
    println!(
        "{:<24} {:<9} {:<14} {:<7} {:<10} {}",
        "project", "source", "branch", "state", "hosts", "remote"
    );
    for project in projects {
        let source = match (project.declared, project.present) {
            (true, true) => "declared",
            (true, false) => "missing",
            (false, true) => "found",
            (false, false) => "-",
        };
        let branch = project
            .git
            .as_ref()
            .map(|git| git.branch.as_str())
            .unwrap_or("-");
        let state = match &project.git {
            Some(git) if git.dirty => "dirty",
            Some(_) => "clean",
            None => "-",
        };
        let mut flags = String::new();
        if project.marbles {
            flags.push_str(" [marbles]");
        }
        if project.archived {
            flags.push_str(" [archived]");
        }
        let mut hosts = Vec::new();
        if project.github.is_some() {
            hosts.push("github");
        }
        if project.gitea.is_some() {
            hosts.push("gitea");
        }
        println!(
            "{:<24} {:<9} {:<14} {:<7} {:<10} {}{}",
            project.name,
            source,
            branch,
            state,
            hosts.join("+"),
            project
                .git
                .as_ref()
                .and_then(|git| git.remote.clone())
                .unwrap_or_default(),
            flags
        );
    }
}

/// `mush projects [--desktop ID] [--json]`
pub fn run(config: &crate::config::Config, mut args: Vec<String>) -> Result<()> {
    let json_output = args.iter().any(|arg| arg == "--json");
    args.retain(|arg| arg != "--json");
    let remote_mode = args.iter().any(|arg| arg == "--remote");
    args.retain(|arg| arg != "--remote");
    // `--claim a b c` groups repositories into this Desktop.
    let claims = match args.iter().position(|arg| arg == "--claim") {
        Some(index) => args.split_off(index + 1),
        None => Vec::new(),
    };
    let desktop_id = crate::take_option(&mut args, "--desktop")
        .or_else(|| std::env::var("MUSH_DESKTOP").ok())
        .or_else(|| config.desktop.clone())
        .or_else(|| {
            // No Desktop named: ask the caller's origin, exactly as `mush` does.
            let cwd = std::env::current_dir().ok()?;
            crate::origin::resolve(&cwd).desktop
        });
    let Some(id) = desktop_id else {
        println!("no Desktop selected and this directory belongs to none");
        std::process::exit(3);
    };
    let desktop = desktop::load(&id)?;

    if !claims.is_empty() {
        for name in &claims {
            desktop::add_project(&id, name)?;
        }
        println!("claimed {} in {id}", claims.join(", "));
        return Ok(());
    }

    let mut projects = discover(&desktop);
    if remote_mode {
        let merged = remotes()?;
        let mut seen = std::collections::BTreeSet::new();
        for project in projects.iter_mut() {
            let key = project.name.to_lowercase();
            seen.insert(key.clone());
            if let Some(remote) = merged.get(&key) {
                project.github = remote.github.clone();
                project.gitea = remote.gitea.clone();
                project.archived = remote.archived;
            }
        }
        for (key, remote) in merged {
            if seen.contains(&key) {
                continue;
            }
            projects.push(Project {
                name: remote.name,
                declared: false,
                present: false,
                git: None,
                marbles: false,
                path: None,
                github: remote.github,
                gitea: remote.gitea,
                archived: remote.archived,
            });
        }
        projects.sort_by_key(|project| project.name.to_lowercase());
    }
    render(&projects, json_output);
    Ok(())
}

/// Clone a repository into a Desktop, shallow, then optionally give it a
/// copy-on-write worktree.
///
/// The clone is `--depth 1` on purpose: this is a starting point for work, not
/// an archive. A worktree is a CoW copy of that shallow clone, which is why it
/// costs almost nothing on a filesystem that supports reflinks and falls back
/// to a plain copy when it does not -- an honest copy is better than a clone
/// that silently takes minutes.
pub fn add(config: &crate::config::Config, mut args: Vec<String>) -> Result<()> {
    let worktree = crate::take_option(&mut args, "--worktree");
    args.retain(|arg| arg != "--json");
    let desktop_id = crate::take_option(&mut args, "--desktop")
        .or_else(|| std::env::var("MUSH_DESKTOP").ok())
        .or_else(|| config.desktop.clone())
        .or_else(|| {
            let cwd = std::env::current_dir().ok()?;
            crate::origin::resolve(&cwd).desktop
        })
        .ok_or_else(|| anyhow::anyhow!("no Desktop selected; pass --desktop ID"))?;
    let desktop = desktop::load(&desktop_id)?;
    let Some(name) = args.first().cloned() else {
        anyhow::bail!("usage: mush project add NAME [--desktop ID] [--worktree LABEL]");
    };

    let projects = desktop.mount.join("projects");
    std::fs::create_dir_all(&projects)
        .map_err(|error| anyhow::anyhow!("create {}: {error}", projects.display()))?;
    let target = projects.join(&name);

    desktop::add_project(&desktop_id, &name)?;
    println!("claimed {name} in {desktop_id}");

    if target.join(".git").exists() {
        println!("already cloned: {}", target.display());
    } else {
        let status = Command::new("gh")
            .arg("repo")
            .arg("clone")
            .arg(&name)
            .arg(&target)
            .arg("--")
            .arg("--depth")
            .arg("1")
            .status()
            .map_err(|error| anyhow::anyhow!("run gh repo clone: {error}"))?;
        if !status.success() {
            anyhow::bail!("gh repo clone {name} failed with {status}");
        }
        println!("cloned (depth 1): {}", target.display());
    }

    if let Some(label) = worktree {
        let base = desktop.mount.join("worktrees").join(&label);
        std::fs::create_dir_all(&base)
            .map_err(|error| anyhow::anyhow!("create {}: {error}", base.display()))?;
        let destination = base.join(&name);
        if destination.exists() {
            println!("worktree already present: {}", destination.display());
            return Ok(());
        }
        cow_copy(&target, &destination)?;
        println!(
            "worktree ({}) {} -> {}",
            cow_kind(),
            target.display(),
            destination.display()
        );
    }
    Ok(())
}

/// `cp -c` is a clonefile on macOS; `--reflink=auto` is the same idea on Linux.
/// Both fall back to a normal copy rather than failing.
fn cow_copy(source: &Path, destination: &Path) -> Result<()> {
    let status = Command::new("cp")
        .arg(cow_flag())
        .arg("-R")
        .arg(source)
        .arg(destination)
        .status()
        .map_err(|error| anyhow::anyhow!("run cp: {error}"))?;
    if !status.success() {
        anyhow::bail!(
            "copy {} -> {} failed",
            source.display(),
            destination.display()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn cow_flag() -> &'static str {
    "-c"
}

#[cfg(not(target_os = "macos"))]
fn cow_flag() -> &'static str {
    "--reflink=auto"
}

#[cfg(target_os = "macos")]
fn cow_kind() -> &'static str {
    "clonefile"
}

#[cfg(not(target_os = "macos"))]
fn cow_kind() -> &'static str {
    "reflink"
}
