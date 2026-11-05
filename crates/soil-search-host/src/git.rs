//! Bounded local Git repository discovery for global search.
//!
//! The host explicitly grants roots. Only repository metadata is read; this
//! never runs Git, opens the network, or indexes working-tree bytes.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const MAX_DEPTH: usize = 6;
const MAX_REPOS: usize = 2048;
const SKIP: &[&str] = &[
    "target",
    "node_modules",
    ".cache",
    "dist",
    "build",
    "vendor",
    ".worktrees",
];

#[derive(Clone, Debug)]
pub struct GitRepo {
    pub id: String,
    pub name: String,
    pub branch: Option<String>,
    pub remote: Option<String>,
    pub recent_branches: Vec<String>,
    pub worktrees: Vec<Worktree>,
}

#[derive(Clone, Debug)]
pub struct Worktree {
    pub id: String,
    pub branch: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RepoAction {
    pub id: String,
    pub title: String,
    pub subtitle: String,
}

#[derive(Default)]
pub struct GitIndex {
    roots: Vec<PathBuf>,
    repos: Vec<GitRepo>,
}

impl GitIndex {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            repos: Vec::new(),
        }
    }

    pub fn refresh(&mut self) {
        self.repos.clear();
        let mut seen = BTreeSet::new();
        for root in &self.roots {
            let mut found = Vec::new();
            discover(root, root, 0, &mut found);
            for repo in found {
                if seen.insert(repo.id.clone()) {
                    self.repos.push(repo);
                }
            }
        }
        self.repos.sort_by(|a, b| a.id.cmp(&b.id));
        self.repos.truncate(MAX_REPOS);
    }

    #[cfg(test)]
    pub fn repos(&self) -> &[GitRepo] {
        &self.repos
    }

    pub fn actions(&self, query: &str, limit: usize) -> Vec<(RepoAction, u32)> {
        let needle = query.to_lowercase();
        let mut actions = Vec::new();
        for repo in &self.repos {
            if !repo.name.to_lowercase().contains(&needle)
                && !repo.id.to_lowercase().contains(&needle)
            {
                continue;
            }
            let base = repo.remote.as_deref().unwrap_or(&repo.id);
            actions.push((
                RepoAction {
                    id: format!("git.open:{}", repo.id),
                    title: format!("Open {}", repo.name),
                    subtitle: base.to_string(),
                },
                950_000,
            ));
            for worktree in &repo.worktrees {
                actions.push((
                    RepoAction {
                        id: format!("git.worktree:{}:{}", repo.id, worktree.id),
                        title: format!(
                            "Open worktree {}",
                            worktree.branch.as_deref().unwrap_or(&worktree.id)
                        ),
                        subtitle: repo.id.clone(),
                    },
                    940_000,
                ));
            }
            for branch in repo.recent_branches.iter().take(8) {
                actions.push((
                    RepoAction {
                        id: format!("git.branch:{}:{}", repo.id, branch),
                        title: format!("Switch to {branch}"),
                        subtitle: repo.id.clone(),
                    },
                    900_000,
                ));
            }
        }
        actions.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
        actions.truncate(limit);
        actions
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(GitRepo, u32)> {
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        for repo in &self.repos {
            let haystacks = [
                repo.name.to_lowercase(),
                repo.id.to_lowercase(),
                repo.branch.as_deref().unwrap_or_default().to_lowercase(),
                repo.remote.as_deref().unwrap_or_default().to_lowercase(),
            ];
            if !haystacks.iter().any(|value| value.contains(&needle)) {
                continue;
            }
            let exact = haystacks.iter().any(|value| value == &needle);
            let prefix = haystacks.iter().any(|value| value.starts_with(&needle));
            let score = if exact {
                1_200_000
            } else if prefix {
                1_100_000
            } else {
                1_000_000
            };
            hits.push((repo.clone(), score));
        }
        hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
        hits.truncate(limit);
        hits
    }
}

fn discover(root: &Path, current: &Path, depth: usize, out: &mut Vec<GitRepo>) {
    if out.len() >= MAX_REPOS || depth > MAX_DEPTH {
        return;
    }
    if current.join(".git").is_dir() {
        if let Some(repo) = read_repo(root, current) {
            out.push(repo);
        }
        // Do not descend into a repository; its nested checkouts are separate
        // only when they have their own granted root or explicit entry.
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    let mut children: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !SKIP.contains(&name))
        })
        .collect();
    children.sort();
    for child in children {
        discover(root, &child, depth + 1, out);
    }
}

fn read_repo(root: &Path, path: &Path) -> Option<GitRepo> {
    let id = path
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    if id.is_empty() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().into_owned();
    let git = path.join(".git");
    let branch = fs::read_to_string(git.join("HEAD"))
        .ok()
        .and_then(|head| {
            head.strip_prefix("ref: refs/heads/")
                .map(|branch| branch.trim().to_string())
                .or_else(|| Some(head.trim().chars().take(8).collect()))
        })
        .filter(|branch| !branch.is_empty());
    let remote = fs::read_to_string(git.join("config"))
        .ok()
        .and_then(|config| parse_origin(&config));
    Some(GitRepo {
        id,
        name,
        branch,
        remote,
        recent_branches: read_recent_branches(&git),
        worktrees: read_worktrees(&git),
    })
}

fn read_recent_branches(git: &Path) -> Vec<String> {
    let mut branches = Vec::new();
    if let Ok(log) = fs::read_to_string(git.join("logs/HEAD")) {
        for line in log.lines().rev() {
            let Some((_, rest)) = line.split_once("\t") else {
                continue;
            };
            for part in rest.split_whitespace() {
                if let Some(branch) = part.strip_prefix("refs/heads/")
                    && !branches.iter().any(|seen| seen == branch)
                {
                    branches.push(branch.to_string());
                }
            }
            if let Some((from, to)) = rest
                .strip_prefix("checkout: moving from ")
                .and_then(|rest| rest.split_once(" to "))
            {
                for branch in [from, to] {
                    if !branch.is_empty() && !branches.iter().any(|seen| seen == branch) {
                        branches.push(branch.to_string());
                    }
                }
            }
            if branches.len() >= 8 {
                break;
            }
        }
    }
    branches
}

fn read_worktrees(git: &Path) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let Ok(entries) = fs::read_dir(git.join("worktrees")) else {
        return worktrees;
    };
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let branch = fs::read_to_string(entry.path().join("HEAD"))
            .ok()
            .and_then(|head| {
                head.strip_prefix("ref: refs/heads/")
                    .map(str::trim)
                    .map(str::to_owned)
            });
        worktrees.push(Worktree { id, branch });
    }
    worktrees.sort_by(|a, b| a.id.cmp(&b.id));
    worktrees
}

fn parse_origin(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
            continue;
        }
        if in_origin && let Some(url) = line.strip_prefix("url = ") {
            let url = sanitize_url(url.trim());
            if !url.is_empty() {
                return Some(url);
            }
        }
    }
    None
}

fn sanitize_url(url: &str) -> String {
    // Drop credentials from scheme URLs without changing the locator shape.
    if let Some((scheme, rest)) = url.split_once("://")
        && let Some((_, host)) = rest.rsplit_once('@')
    {
        return format!("{scheme}://{host}");
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_repos_and_sanitizes_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repo = root.join("fpl/dock");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir_all(repo.join(".git/worktrees/feat")).unwrap();
        fs::write(
            repo.join(".git/worktrees/feat/HEAD"),
            "ref: refs/heads/feat/ui\n",
        )
        .unwrap();
        fs::create_dir_all(repo.join(".git/logs")).unwrap();
        fs::write(
            repo.join(".git/logs/HEAD"),
            "0 0\tcheckout: moving from main to main\n",
        )
        .unwrap();
        fs::write(
            repo.join(".git/config"),
            "[remote \"origin\"]\n\turl = https://user:token@github.com/acme/dock.git\n",
        )
        .unwrap();
        let ignored = root.join("target/nope");
        fs::create_dir_all(ignored.join(".git")).unwrap();
        let mut index = GitIndex::new(vec![root.to_path_buf()]);
        index.refresh();
        assert_eq!(index.repos().len(), 1);
        let repo = &index.repos()[0];
        assert_eq!(repo.id, "fpl/dock");
        assert_eq!(repo.branch.as_deref(), Some("main"));
        assert_eq!(
            repo.remote.as_deref(),
            Some("https://github.com/acme/dock.git")
        );
        assert_eq!(index.search("dock", 10).len(), 1);
        assert_eq!(index.search("main", 10).len(), 1);
        assert!(index.search("nope", 10).is_empty());
        assert_eq!(repo.worktrees[0].branch.as_deref(), Some("feat/ui"));
        let actions = index.actions("dock", 10);
        assert!(
            actions
                .iter()
                .any(|(action, _)| action.id == "git.open:fpl/dock")
        );
        assert!(
            actions
                .iter()
                .any(|(action, _)| action.id == "git.worktree:fpl/dock:feat")
        );
        assert!(
            actions
                .iter()
                .any(|(action, _)| action.id == "git.branch:fpl/dock:main")
        );
    }
}
