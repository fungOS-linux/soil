//! Resolve where a Delta-originated caller should work.
//!
//! A Delta agent starts inside one of its worktrees with only two hints about
//! where it came from: `DELTA_CURRENT_THREAD_ID` and a working directory shaped
//! like `<repo>/.delta/worktrees/<id>/<repo>`. This module turns those into a
//! Desktop and the path that Desktop is projected at.
//!
//! It resolves; it does not mount. Reporting `NotMounted` is the point: handing
//! back a path where nothing is mounted would send an agent's writes into an
//! empty directory that looks exactly like success.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

use crate::desktop;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The Desktop is registered and its projection is a live mount.
    Ready,
    /// A Desktop matched, but nothing is mounted at its projection path.
    NotMounted,
    /// The projection is mounted, but this repository is not under it.
    NoProject,
    /// No Desktop claims this repository or this path.
    Unregistered,
}

impl Readiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Readiness::Ready => "ready",
            Readiness::NotMounted => "not-mounted",
            Readiness::NoProject => "no-project",
            Readiness::Unregistered => "unregistered",
        }
    }
}

#[derive(Debug)]
pub struct Origin {
    pub cwd: PathBuf,
    pub thread: Option<String>,
    pub delta_worktree: Option<String>,
    /// `<mount>/worktrees/<label>/<project>`: this agent's own tree.
    pub worktree: Option<String>,
    /// Marquee identity from `.marbles/project.toml`, when the project has one.
    pub marble_project: Option<String>,
    pub repo: Option<String>,
    pub desktop: Option<String>,
    pub desktop_root: Option<PathBuf>,
    pub mount: Option<PathBuf>,
    pub project: Option<PathBuf>,
    pub readiness: Readiness,
}

impl Origin {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "cwd": self.cwd.display().to_string(),
            "thread": self.thread,
            "delta_worktree": self.delta_worktree,
            "worktree": self.worktree,
            "marble_project": self.marble_project,
            "repo": self.repo,
            "desktop": self.desktop,
            "desktop_root": self.desktop_root.as_ref().map(|path| path.display().to_string()),
            "mount": self.mount.as_ref().map(|path| path.display().to_string()),
            "project": self.project.as_ref().map(|path| path.display().to_string()),
            "readiness": self.readiness.as_str(),
        })
    }
}

/// Resolve `cwd` against the Desktop registry and the live mount table.
pub fn resolve(cwd: &Path) -> Origin {
    let thread = std::env::var("DELTA_CURRENT_THREAD_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let delta_worktree = delta_worktree_id(cwd);
    let desktops = desktop::list().unwrap_or_default();
    let mount_match = desktops
        .iter()
        .find(|desktop| path_is_under(cwd, &desktop.mount));
    // Three ways to learn the repository name, strongest first: Git metadata,
    // the Delta worktree directory, then the caller's place inside a Desktop's
    // `projects/` tree.
    // A Desktop worktree is `<mount>/worktrees/<label>/<project>`: the label is
    // the agent's own tree, the project is what it is a tree of.
    let worktree_info = mount_match.and_then(|desktop| worktree_segment(cwd, &desktop.mount));
    let worktree = worktree_info.as_ref().map(|(label, _)| label.clone());

    let repo = repo_name(cwd)
        .or_else(|| worktree_info.as_ref().map(|(_, project)| project.clone()))
        .or_else(|| {
            delta_worktree.as_ref().and_then(|_| {
                cwd.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            })
        })
        .or_else(|| mount_match.and_then(|desktop| project_segment(cwd, &desktop.mount)));

    // The marble project marker travels with the tree, so whichever project the
    // agent is inside answers this.
    let marble_project = marble_project_from(cwd);

    let matched = mount_match.or_else(|| {
        repo.as_deref().and_then(|repo| {
            desktops
                .iter()
                .find(|desktop| desktop.projects.iter().any(|project| project == repo))
        })
    });

    let Some(desktop) = matched else {
        return Origin {
            cwd: cwd.to_path_buf(),
            thread,
            delta_worktree,
            worktree: None,
            marble_project: None,
            repo,
            desktop: None,
            desktop_root: None,
            mount: None,
            project: None,
            readiness: Readiness::Unregistered,
        };
    };

    let mount = desktop.mount.clone();
    let project = repo
        .as_deref()
        .map(|repo| mount.join("projects").join(repo));
    let readiness = if !is_mount_point(&mount) {
        Readiness::NotMounted
    } else if !project.as_deref().is_some_and(Path::exists) {
        Readiness::NoProject
    } else {
        Readiness::Ready
    };

    Origin {
        cwd: cwd.to_path_buf(),
        thread,
        delta_worktree,
        worktree,
        marble_project,
        repo,
        desktop: Some(desktop.id.clone()),
        desktop_root: Some(desktop.root.clone()),
        mount: Some(mount),
        project,
        readiness,
    }
}

/// The worktree encoder in `<repo>/.delta/worktrees/<id>/<repo>`.
fn delta_worktree_id(cwd: &Path) -> Option<String> {
    let mut components = cwd.components().map(|component| component.as_os_str());
    while let Some(part) = components.next() {
        if part == ".delta" && components.next() == Some("worktrees".as_ref()) {
            return components
                .next()
                .and_then(|id| id.to_str())
                .map(str::to_string);
        }
    }
    None
}

/// Repository name: the nearest Git root, or the directory itself.
fn repo_name(cwd: &Path) -> Option<String> {
    let mut current = cwd;
    loop {
        if current.join(".git").exists() {
            return current
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string);
        }
        current = current.parent()?;
    }
}

/// `<mount>/worktrees/<label>/<project>` -> (label, project).
fn worktree_segment(cwd: &Path, mount: &Path) -> Option<(String, String)> {
    let (cwd, mount) = (canonical(cwd), canonical(mount));
    let mut parts = cwd.strip_prefix(&mount).ok()?.components();
    if parts.next()?.as_os_str() != "worktrees" {
        return None;
    }
    let label = parts.next()?.as_os_str().to_str()?.to_string();
    let project = parts.next()?.as_os_str().to_str()?.to_string();
    Some((label, project))
}

/// The marbles project slug nearest to `cwd`, if the tree carries a marker.
fn marble_project_from(cwd: &Path) -> Option<String> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        for marker in [".marbles", ".marbles-local"] {
            let path = dir.join(marker).join("project.toml");
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                if let Some(value) = line.trim().strip_prefix("slug") {
                    let slug = value
                        .trim_start_matches([' ', '='])
                        .trim()
                        .trim_matches('"');
                    if !slug.is_empty() {
                        return Some(slug.to_string());
                    }
                }
            }
        }
        current = dir.parent();
    }
    None
}

/// Compare real paths: `/var` is a symlink to `/private/var` on macOS, and a
/// configured mount is written the short way while `current_dir` reports the
/// resolved way. Comparing the strings alone silently unregisters every
/// Desktop under a symlinked path.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_is_under(path: &Path, base: &Path) -> bool {
    canonical(path).starts_with(canonical(base))
}

/// The project name in `<mount>/projects/<name>/...`.
fn project_segment(cwd: &Path, mount: &Path) -> Option<String> {
    let (cwd, mount) = (canonical(cwd), canonical(mount));
    let mut parts = cwd.strip_prefix(&mount).ok()?.components();
    if parts.next()?.as_os_str() != "projects" {
        return None;
    }
    parts.next()?.as_os_str().to_str().map(str::to_string)
}

/// Whether something is mounted at `path`.
///
/// A mount point is a directory whose device differs from its parent's. That
/// covers NFS and FUSE projections without asking the platform which is which.
fn is_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return Path::exists(path);
    };
    match (std::fs::metadata(path), std::fs::metadata(parent)) {
        (Ok(here), Ok(up)) => here.dev() != up.dev(),
        _ => false,
    }
}

/// Print a resolution for a human, or the JSON form when asked.
pub fn run(mut args: Vec<String>) -> Result<()> {
    let cwd = match crate::take_option(&mut args, "--cwd") {
        Some(value) => crate::config::expand_home(&value),
        None => std::env::current_dir()?,
    };
    let as_json = args.iter().any(|arg| arg == "--json");
    let origin = resolve(&cwd);
    if as_json {
        println!("{}", serde_json::to_string(&origin.to_json())?);
    } else {
        println!("cwd: {}", origin.cwd.display());
        println!("thread: {}", origin.thread.as_deref().unwrap_or("(none)"));
        println!(
            "delta worktree: {}",
            origin.delta_worktree.as_deref().unwrap_or("(none)")
        );
        println!(
            "worktree: {}",
            origin.worktree.as_deref().unwrap_or("(none)")
        );
        println!(
            "marble project: {}",
            origin.marble_project.as_deref().unwrap_or("(none)")
        );
        println!("repo: {}", origin.repo.as_deref().unwrap_or("(none)"));
        println!(
            "desktop: {}",
            origin.desktop.as_deref().unwrap_or("(unregistered)")
        );
        if let Some(root) = &origin.desktop_root {
            println!("vfs root: {}", root.display());
        }
        if let Some(mount) = &origin.mount {
            println!(
                "mount: {} [{}]",
                mount.display(),
                if is_mount_point(mount) {
                    "mounted"
                } else {
                    "not mounted"
                }
            );
        }
        if let Some(project) = &origin.project {
            println!("project: {}", project.display());
        }
        println!("status: {}", origin.readiness.as_str());
    }
    match origin.readiness {
        Readiness::Ready => Ok(()),
        Readiness::NotMounted | Readiness::NoProject => std::process::exit(2),
        Readiness::Unregistered => std::process::exit(3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_delta_worktree_encoder_from_the_path() {
        let cwd = Path::new("/src/soil/.delta/worktrees/njtb5mrzj3h2/soil");
        assert_eq!(delta_worktree_id(cwd).as_deref(), Some("njtb5mrzj3h2"));
        assert_eq!(delta_worktree_id(Path::new("/src/soil")), None);
    }

    #[test]
    fn reads_the_project_name_from_a_path_inside_the_mount() {
        let mount = Path::new("/home/me/mnt/personal");
        assert_eq!(
            project_segment(Path::new("/home/me/mnt/personal/projects/soil/src"), mount).as_deref(),
            Some("soil")
        );
        // A path elsewhere in the mount is not a project.
        assert_eq!(
            project_segment(Path::new("/home/me/mnt/personal/notes"), mount),
            None
        );
        assert_eq!(project_segment(Path::new("/tmp/elsewhere"), mount), None);
    }

    #[test]
    fn a_path_outside_any_registered_mount_is_unregistered() {
        let origin = resolve(Path::new("/tmp/not-a-desktop-anywhere"));
        assert_eq!(origin.readiness, Readiness::Unregistered);
        assert_eq!(origin.desktop, None);
    }
}
