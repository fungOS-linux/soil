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
            }
        })
        .collect()
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
        "{:<24} {:<9} {:<14} {:<7} {}",
        "project", "source", "branch", "state", "remote"
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
        println!(
            "{:<24} {:<9} {:<14} {:<7} {}{}",
            project.name,
            source,
            branch,
            state,
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
    let projects = discover(&desktop);
    render(&projects, json_output);
    Ok(())
}
