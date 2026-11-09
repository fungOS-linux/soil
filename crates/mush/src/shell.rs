//! `mush sh`: one command line, with the repository toolchain allowed through.
//!
//! The Spore has no host process. That is what keeps it safe, and it is also
//! what stops it from being an agent's shell: `git` and `cargo` live on the
//! other side of that boundary. This module runs an allowlisted host program
//! against an overlay of the VFS -- materialize, run, apply the diff back -- so
//! the toolchain works without giving the Spore a process.
//!
//! The allowlist is deliberately short. A host program runs with the user's
//! full privileges and nothing but the overlay as its working directory: it can
//! still read `$HOME`, so anything that talks to the network or holds
//! credentials belongs here only after a decision, not by default.

use std::{path::Path, process::Command};

use anyhow::{Context, Result};

use crate::{Runtime, overlay::Overlay};

/// Bare program names that may run as host processes.
///
/// Compilers, version control, and text tools: they see the overlay and nothing
/// of the VFS. `gh` is here by decision, not by default -- it carries the
/// user's credentials and network access, so a run can push a branch and open a
/// PR. It cannot merge: `fab` owns checks and the landing decision, and an
/// allowlisted `gh` is the shortest path from a prompt to something reviewable
/// without putting a merge button behind a tool call.
const HOST_ALLOWLIST: &[&str] = &[
    "cargo",
    "rustc",
    "cc",
    "clang",
    "clang++",
    "c++",
    "git",
    "gh",
    "make",
    "cmake",
    "ninja",
    "pkg-config",
    "rg",
    "sed",
    "awk",
    "diff",
    "jq",
    "true",
    "false",
    "test",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Connector {
    First,
    And,
    Or,
    Always,
}

/// Run a whole command line, chaining on `&&`, `||` and `;`.
pub fn run_line(runtime: &mut Runtime, root: &Path, line: &str) -> Result<i32> {
    let mut status = 0;
    for (connector, command) in split_commands(line) {
        let should_run = match connector {
            Connector::And => status == 0,
            Connector::Or => status != 0,
            Connector::First | Connector::Always => true,
        };
        if !should_run {
            continue;
        }
        status = run_one(runtime, root, &command)?;
    }
    Ok(status)
}

fn run_one(runtime: &mut Runtime, root: &Path, command: &str) -> Result<i32> {
    // Redirection and pipelines are the Spore's job; hand it the whole line so
    // its parser sees the operators it understands.
    let words = split_words(command);
    if words.is_empty() {
        return Ok(0);
    }
    if command.contains('|') || command.contains('>') || command.contains('<') {
        return run_in_spore(runtime, command);
    }
    if host_allowed(&words[0]) {
        return run_host(root, &words[0], &words[1..]);
    }
    run_in_spore(runtime, command)
}

fn run_in_spore(runtime: &mut Runtime, command: &str) -> Result<i32> {
    let executed = runtime.execute(command)?;
    print!("{}", executed.output);
    eprint!("{}", executed.stderr);
    Ok(executed.status)
}

/// Materialize the VFS, run the program there, apply whatever it changed.
fn run_host(root: &Path, program: &str, args: &[String]) -> Result<i32> {
    let overlay = Overlay::materialize(root)
        .with_context(|| format!("materialize a workspace for {program}"))?;
    let mut command = Command::new(program);
    command.args(args).current_dir(overlay.path());
    // Whoever is running also knows who it is running as. Passing that to the
    // child is what lets a tool record *which* agent claimed a piece of work
    // instead of leaving the link to be reconstructed later.
    for (key, value) in identity(root) {
        command.env(key, value);
    }
    let status = command.status().with_context(|| format!("run {program}"))?;
    overlay
        .apply(root)
        .with_context(|| format!("apply what {program} changed"))?;
    Ok(status.code().unwrap_or(1))
}

/// The identity of the caller, as environment for a host program.
///
/// Empty where the thing is unknown: a half-known identity is worse than an
/// absent one, because a consumer cannot tell which half is a guess.
fn identity(root: &Path) -> Vec<(String, String)> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| root.to_path_buf());
    let origin = crate::origin::resolve(&cwd);
    let mut out = Vec::new();
    if let Some(thread) = origin.thread {
        out.push(("MUSH_THREAD_ID".to_string(), thread));
    }
    if let Some(worktree) = origin.worktree {
        out.push(("MUSH_WORKTREE".to_string(), worktree));
    }
    if let Some(project) = origin.marble_project {
        out.push(("MUSH_MARBLE_PROJECT".to_string(), project));
    }
    if let Some(desktop) = origin.desktop {
        out.push(("MUSH_DESKTOP".to_string(), desktop));
    }
    if let Some(repo) = origin.repo {
        out.push(("MUSH_PROJECT".to_string(), repo));
    }
    out
}

/// A bare program name only. `./cargo` would run code from the overlay itself,
/// which is exactly what the allowlist exists to prevent.
fn host_allowed(program: &str) -> bool {
    !program.contains('/') && HOST_ALLOWLIST.contains(&program)
}

fn split_commands(line: &str) -> Vec<(Connector, String)> {
    let mut out = Vec::new();
    let mut buffer = String::new();
    let mut connector = Connector::First;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                buffer.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    buffer.push(c);
                }
                ';' => {
                    out.push((connector, buffer.trim().to_string()));
                    buffer.clear();
                    connector = Connector::Always;
                }
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    out.push((connector, buffer.trim().to_string()));
                    buffer.clear();
                    connector = Connector::And;
                }
                '|' if chars.peek() == Some(&'|') => {
                    chars.next();
                    out.push((connector, buffer.trim().to_string()));
                    buffer.clear();
                    connector = Connector::Or;
                }
                _ => buffer.push(c),
            },
        }
    }
    out.push((connector, buffer.trim().to_string()));
    out.retain(|(_, command)| !command.is_empty());
    out
}

fn split_words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buffer = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    buffer.push(c);
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                c if c.is_whitespace() => {
                    if !buffer.is_empty() {
                        out.push(std::mem::take(&mut buffer));
                    }
                }
                _ => buffer.push(c),
            },
        }
    }
    if !buffer.is_empty() {
        out.push(buffer);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_chains_and_keeps_quoted_operators() {
        // The connector rides on the command it gates: `b` follows `;`, `c`
        // follows `&&`, `d` follows `||`.
        let parts = split_commands("a; b && c || d");
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].0, Connector::First);
        assert_eq!(parts[1].0, Connector::Always);
        assert_eq!(parts[2].0, Connector::And);
        assert_eq!(parts[3].0, Connector::Or);
        let quoted = split_commands("echo 'a && b'");
        assert_eq!(quoted.len(), 1);
    }

    #[test]
    fn splits_words_without_quotes() {
        assert_eq!(
            split_words("git commit -m 'hi there'"),
            vec!["git", "commit", "-m", "hi there"]
        );
        assert!(split_words("   ").is_empty());
    }

    #[test]
    fn a_path_is_never_a_host_program() {
        assert!(host_allowed("git"));
        assert!(!host_allowed("./git"));
        // Credential-bearing, but allowed by decision so a run can open a PR.
        assert!(host_allowed("gh"));
        // A random binary is still refused.
        assert!(!host_allowed("python3"));
    }
}
