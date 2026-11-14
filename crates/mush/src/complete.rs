//! Interactive completion for the Mush host.
//!
//! Completion runs in the host rather than the Spore: only the host can see the
//! installed command packages, and only the host owns the readline session. It
//! completes the two things it can be sure about -- command names, and VFS paths
//! relative to the shell's current directory, which the loop keeps fresh because
//! the Spore owns `cd`.

use std::{
    borrow::Cow,
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use rustyline::{
    Context, Helper, Result as RustylineResult,
    completion::{Completer, Pair},
    highlight::Highlighter,
    hint::Hinter,
    validate::Validator,
};

/// Mirrors the builtin dispatch in `spores/mush/src/lib.rs`.
///
/// This duplicates the Spore's list on purpose: the Spore has no completion
/// export yet, and a host that completes nothing is worse than a host with a
/// list to update. When the Spore grows a `complete` export, delete this.
const BUILTINS: &[&str] = &[
    "append", "break", "cat", "cd", "commands", "continue", "cp", "echo", "env", "exit", "export",
    "false", "find", "grep", "head", "help", "ls", "mkdir", "mv", "printf", "pwd", "read", "rm",
    "rmdir", "search", "set", "source", "stat", "tail", "test", "tgrep", "touch", "tree", "true",
    "unset", "wc", "which", "write",
];

pub struct MushHelper {
    root: PathBuf,
    cwd: Arc<Mutex<String>>,
    names: Vec<String>,
}

impl MushHelper {
    pub fn new(root: PathBuf, roots: &[PathBuf]) -> Self {
        let mut names: Vec<String> = BUILTINS.iter().map(|name| (*name).to_string()).collect();
        if let Ok(registry) = soil_command::CommandRegistry::scan_roots(roots) {
            names.extend(registry.entries().map(|entry| entry.name.clone()));
        }
        names.sort();
        names.dedup();
        Self {
            root,
            cwd: Arc::new(Mutex::new("/".into())),
            names,
        }
    }

    /// Shared handle so the readline loop can keep the shell's cwd current.
    pub fn cwd_handle(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.cwd)
    }

    fn cwd(&self) -> String {
        self.cwd
            .lock()
            .map(|cwd| cwd.clone())
            .unwrap_or_else(|_| "/".into())
    }

    fn path_candidates(&self, word: &str) -> Vec<Pair> {
        let (dir, prefix) = match word.rfind('/') {
            Some(index) => (word[..index].to_string(), word[index + 1..].to_string()),
            None => (self.cwd(), word.to_string()),
        };
        let dir = self.resolve(&dir);
        let base = if dir == "/" {
            "/".to_string()
        } else {
            format!("{}/", dir.trim_end_matches('/'))
        };
        let Ok(vfs) = fungos_vfs::Vfs::open(&self.root) else {
            return Vec::new();
        };
        let Ok(entries) = vfs.list(&base) else {
            return Vec::new();
        };
        // `list` returns files, so a directory is implied by a deeper segment.
        let mut seen = BTreeSet::new();
        for entry in entries {
            let Some(rest) = entry.path.strip_prefix(&base) else {
                continue;
            };
            let segment = rest.split('/').next().unwrap_or(rest);
            if segment.is_empty() || !segment.starts_with(&prefix) {
                continue;
            }
            seen.insert((segment.to_string(), rest.contains('/')));
        }
        seen.into_iter()
            .map(|(name, is_dir)| {
                let text = if is_dir { format!("{name}/") } else { name };
                Pair {
                    display: text.clone(),
                    replacement: text,
                }
            })
            .collect()
    }

    fn resolve(&self, dir: &str) -> String {
        if dir.is_empty() || dir == "." {
            return self.cwd();
        }
        if dir.starts_with('/') {
            return normalize(dir);
        }
        normalize(&format!("{}/{}", self.cwd(), dir))
    }
}

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

impl Helper for MushHelper {}

impl Validator for MushHelper {}

impl Hinter for MushHelper {
    type Hint = String;
}

impl Highlighter for MushHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        Cow::Borrowed(line)
    }
}

impl Completer for MushHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> RustylineResult<(usize, Vec<Pair>)> {
        let start = line[..pos]
            .rfind(char::is_whitespace)
            .map(|index| index + 1)
            .unwrap_or(0);
        let word = &line[start..pos];
        let first_word = line[..start].trim().is_empty();

        if first_word && !word.contains('/') {
            let candidates = self
                .names
                .iter()
                .filter(|name| name.starts_with(word))
                .map(|name| Pair {
                    display: name.clone(),
                    replacement: name.clone(),
                })
                .collect();
            return Ok((start, candidates));
        }
        Ok((start, self.path_candidates(word)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_absolute_paths() {
        let helper = MushHelper {
            root: PathBuf::from("/nonexistent"),
            cwd: Arc::new(Mutex::new("/projects".into())),
            names: Vec::new(),
        };
        assert_eq!(helper.resolve("notes"), "/projects/notes");
        assert_eq!(helper.resolve("../scratch"), "/scratch");
        assert_eq!(helper.resolve("/"), "/");
        assert_eq!(helper.resolve("."), "/projects");
    }
}
