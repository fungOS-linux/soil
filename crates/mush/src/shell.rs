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
///
/// This is the only copy of the policy: the daemon, the interactive shell and
/// `mush run host` all read it from here.
pub const HOST_ALLOWLIST: &[&str] = &[
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

/// Where a command line's output goes.
///
/// An interactive shell writes to the terminal and lets host programs inherit
/// the process's stdio, so prompts and pagers still work. The service daemon
/// captures instead: its output has to travel back over the socket, and a host
/// program that inherits a detached daemon's stdio would lose its output.
pub trait Output {
    fn stdout(&mut self, text: &str);
    fn stderr(&mut self, text: &str);
    /// Whether a host program may use the process's own stdio.
    fn inherits_stdio(&self) -> bool;
}

/// The caller's terminal.
pub struct Terminal;

impl Output for Terminal {
    fn stdout(&mut self, text: &str) {
        print!("{text}");
    }
    fn stderr(&mut self, text: &str) {
        eprint!("{text}");
    }
    fn inherits_stdio(&self) -> bool {
        true
    }
}

/// Buffers for a command that runs inside the daemon.
#[derive(Default)]
pub struct Capture {
    pub out: String,
    pub err: String,
}

impl Output for Capture {
    fn stdout(&mut self, text: &str) {
        self.out.push_str(text);
    }
    fn stderr(&mut self, text: &str) {
        self.err.push_str(text);
    }
    fn inherits_stdio(&self) -> bool {
        false
    }
}


/// What a caller can do to a command that is already running.
///
/// A guest can be interrupted through the engine epoch; a host program is an
/// OS process, so the only way to stop it is to hold its handle. Both live
/// here, because `cancel` and `--timeout` have to mean the same thing whether
/// the command is Spore code or `cargo`.
#[derive(Clone)]
pub struct Control {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    /// Wall-clock budget for one command line, enforced for host programs too.
    budget: Option<std::time::Duration>,
}

impl Control {
    pub fn new(cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            cancel,
            child: std::sync::Arc::new(std::sync::Mutex::new(None)),
            budget: None,
        }
    }

    pub fn with_budget(mut self, budget: Option<std::time::Duration>) -> Self {
        self.budget = budget;
        self
    }

    /// A per-request deadline, so one caller's `--timeout` is not the whole
    /// daemon's.
    pub(crate) fn set_budget(&mut self, budget: Option<std::time::Duration>) {
        self.budget = budget;
    }

    /// True once, if a caller asked for this command to stop.
    fn cancelled(&self) -> bool {
        self.cancel
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    fn track(&self, child: std::process::Child) {
        *self.child.lock().expect("child slot") = Some(child);
    }

    /// Kill the running host program, if there is one.
    pub fn kill(&self) -> bool {
        let Some(mut child) = self.child.lock().expect("child slot").take() else {
            return false;
        };
        let _ = child.kill();
        let _ = child.wait();
        true
    }
}

/// Run a whole command line, chaining on `&&`, `||` and `;`.
pub fn run_line(runtime: &mut Runtime, root: &Path, line: &str) -> Result<i32> {
    let mut terminal = Terminal;
    let control = Control::new(std::sync::Arc::new(Default::default())).with_budget(runtime.budget());
    run_line_with(&mut terminal, runtime, root, line, &control)
}

/// Run a command line and return what it printed, for the service daemon.
pub fn run_line_captured(
    runtime: &mut Runtime,
    root: &Path,
    line: &str,
    control: &Control,
) -> Result<(i32, String, String)> {
    let mut capture = Capture::default();
    let status = run_line_with(&mut capture, runtime, root, line, control)?;
    Ok((status, capture.out, capture.err))
}

fn run_line_with(
    output: &mut dyn Output,
    runtime: &mut Runtime,
    root: &Path,
    line: &str,
    control: &Control,
) -> Result<i32> {
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
        status = run_one(output, runtime, root, &command, control)?;
    }
    Ok(status)
}

fn run_one(
    output: &mut dyn Output,
    runtime: &mut Runtime,
    root: &Path,
    command: &str,
    control: &Control,
) -> Result<i32> {
    // Redirection and pipelines are the Spore's job; hand it the whole line so
    // its parser sees the operators it understands.
    let words = split_words(command);
    if words.is_empty() {
        return Ok(0);
    }
    if command.contains('|') || command.contains('>') || command.contains('<') {
        return run_in_spore(output, runtime, command);
    }
    // A command the Spore can answer exactly (or a host form it covers) never
    // pays for an overlay.
    if let Some(line) = spore_line(command, &words) {
        return run_in_spore(output, runtime, &line);
    }
    if host_allowed(&words[0]) {
        // The Spore owns `cd`; the host program runs on the overlay, so ask
        // where the shell is before starting it.
        let cwd = spore_cwd(runtime);
        return run_host(output, root, &words[0], &words[1..], &cwd, control);
    }
    run_in_spore(output, runtime, command)
}

fn run_in_spore(output: &mut dyn Output, runtime: &mut Runtime, command: &str) -> Result<i32> {
    let executed = runtime.execute(command)?;
    output.stdout(&executed.output);
    output.stderr(&executed.stderr);
    Ok(executed.status)
}

/// The Spore's current directory, relative to the workspace root.
///
/// A host program runs against the overlay, so without this `cd docs && git
/// status` would report on the workspace root. A failed `pwd` is not worth
/// failing the command over: the overlay root is where it would have run
/// anyway.
fn spore_cwd(runtime: &mut Runtime) -> String {
    let Ok(executed) = runtime.execute("pwd") else {
        return String::new();
    };
    executed
        .output
        .trim()
        .trim_start_matches('/')
        .to_string()
}

/// Materialize the VFS, run the program there, apply whatever it changed.
///
/// The child is spawned rather than waited on so a cancel or a deadline can
/// take its handle and kill it: a `cargo build` that ignores the epoch must
/// still be stoppable, or `--timeout` and `mush cancel` would be true only for
/// commands nobody needs to interrupt.
fn run_host(
    output: &mut dyn Output,
    root: &Path,
    program: &str,
    args: &[String],
    cwd: &str,
    control: &Control,
) -> Result<i32> {
    use crate::outcome::{CANCELLED_STATUS, TIMEOUT_STATUS};

    let overlay = Overlay::materialize(root)
        .with_context(|| format!("materialize a workspace for {program}"))?;
    let mut command = Command::new(program);
    command.args(args);
    // `cwd` is the Spore's directory; fall back to the overlay root when it does
    // not name a directory there (a stale session, or a directory the overlay
    // could not hold).
    let start = overlay.path().join(cwd);
    command.current_dir(if start.is_dir() {
        start
    } else {
        overlay.path().to_path_buf()
    });
    // Whoever is running also knows who it is running as. Passing that to the
    // child is what lets a tool record *which* agent claimed a piece of work
    // instead of leaving the link to be reconstructed later.
    for (key, value) in identity(root) {
        command.env(key, value);
    }
    let capturing = !output.inherits_stdio();
    if capturing {
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
    }
    let mut child = command.spawn().with_context(|| format!("run {program}"))?;
    let stdout_reader = capturing.then(|| {
        let pipe = child.stdout.take().expect("piped stdout");
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = std::io::Read::read_to_end(&mut std::io::BufReader::new(pipe), &mut buffer);
            buffer
        })
    });
    let stderr_reader = capturing.then(|| {
        let pipe = child.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = std::io::Read::read_to_end(&mut std::io::BufReader::new(pipe), &mut buffer);
            buffer
        })
    });

    let deadline = control.budget.map(|budget| std::time::Instant::now() + budget);
    control.track(child);

    let mut stopped: Option<i32> = None;
    let status = loop {
        // Poll through the shared slot: a canceller takes the handle away.
        let mut slot = control.child.lock().expect("child slot");
        let finished = match slot.as_mut() {
            None => {
                // The killer took the handle; consume the flag so the next
                // command in this session is not born cancelled.
                control.cancelled();
                Some(stopped.unwrap_or(CANCELLED_STATUS))
            }
            Some(child) => match child.try_wait().with_context(|| format!("wait for {program}"))? {
                Some(status) => {
                    *slot = None;
                    Some(status.code().unwrap_or(1))
                }
                None => None,
            },
        };
        drop(slot);
        if let Some(status) = finished {
            break status;
        }
        let expired = deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if expired || control.cancelled() {
            stopped = Some(if expired { TIMEOUT_STATUS } else { CANCELLED_STATUS });
            control.kill();
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    // The caller sees the same words a guest-interrupted command gets, so
    // "why did this stop" never depends on which lane it ran in.
    if status == CANCELLED_STATUS {
        output.stderr("mush: command cancelled\n");
    } else if status == TIMEOUT_STATUS {
        output.stderr(&crate::deadline::timeout_hint(
            control.budget.unwrap_or_default(),
        ));
    }
    if let Some(reader) = stdout_reader
        && let Ok(bytes) = reader.join()
    {
        output.stdout(&String::from_utf8_lossy(&bytes));
    }
    if let Some(reader) = stderr_reader
        && let Ok(bytes) = reader.join()
    {
        output.stderr(&String::from_utf8_lossy(&bytes));
    }
    overlay
        .apply(root)
        .with_context(|| format!("apply what {program} changed"))?;
    Ok(status)
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
pub fn host_allowed(program: &str) -> bool {
    !program.contains('/') && HOST_ALLOWLIST.contains(&program)
}

/// Names the Spore must win even though the host allowlist also has them.
///
/// The allowlist exists for work the Spore cannot do — a compiler, git over the
/// network. It also carries older conveniences (`jq`, `sed`, `awk`, `rg`) that
/// the Spore has no equivalent for yet, so those keep running as host programs.
/// `diff` and `json` are different: the Spore answers both from the VFS, and the
/// overlay copy would shadow that with a second, unindexed answer that leaves
/// the sandbox to run. `true`, `false` and `test` are shell builtins the host
/// list only ever covered by accident — running `/usr/bin/test` against a
/// materialized copy of the VFS is both slower and asking the wrong filesystem.
/// A name missing here still runs the host program, so forgetting one costs an
/// overlay, not correctness.
const SPORE_OWNED: &[&str] = &["diff", "json", "true", "false", "test"];

fn spore_owned(program: &str) -> bool {
    SPORE_OWNED.contains(&program)
}

/// The Spore command line that answers a host invocation exactly, if there is
/// one.
///
/// Two cases, both about a name the host allowlist also owns: a Spore-owned
/// command, which runs as written, and a `sed -i` substitution the Spore's `sub`
/// already implements, which is rewritten into the equivalent `sub` line.
/// Returning `None` means "run the host program", and that fallback is what
/// keeps a half-supported form from being silently approximated.
pub fn spore_line(line: &str, words: &[String]) -> Option<String> {
    let program = words.first()?;
    if spore_owned(program) {
        return Some(line.to_string());
    }
    if program == "sed" {
        return sed_as_sub(words);
    }
    None
}

/// Translate `sed -i 's/pat/rep/g' FILE...` into `sub pat rep FILE...`.
///
/// Only the subset where the two regex dialects and the two replacement rules
/// agree, because a translation that is subtly wrong is worse than an overlay:
///
/// - `g` is required. Without it sed replaces the first match per line and `sub`
///   replaces every match.
/// - No `\`, `+?|(){}` in either half; sed reads those literally (or as BRE
///   groups) and the Spore's regex reads them as operators. `.`, `*` and
///   bracket classes mean the same in both, and `sub` runs multi-line, so `^`
///   and `$` anchor per line as they do in sed.
/// - No `&` or `$` in the replacement: sed expands `&` to the whole match and
///   `$n` is literal, while `sub` uses `$n` for groups.
/// - No `-i.bak` (we would not write the backup) and no second script.
fn sed_as_sub(words: &[String]) -> Option<String> {
    let mut in_place = false;
    let mut script: Option<&str> = None;
    let mut paths: Vec<&str> = Vec::new();
    let mut index = 1;
    while index < words.len() {
        let arg = words[index].as_str();
        if arg == "-i" {
            in_place = true;
            // BSD sed takes the backup suffix as the next word: `-i ''`.
            if words.get(index + 1).is_some_and(|next| next.is_empty()) {
                index += 1;
            }
        } else if arg == "-e" {
            let value = words.get(index + 1)?;
            if script.is_some() {
                return None;
            }
            script = Some(value);
            index += 1;
        } else if arg != "-" && arg.starts_with('-') {
            return None;
        } else if script.is_none() {
            script = Some(arg);
        } else {
            paths.push(arg);
        }
        index += 1;
    }
    if !in_place || paths.is_empty() {
        return None;
    }
    let (pattern, replacement) = sed_substitution(script?)?;
    let mut line = format!("sub {} {}", quote_word(&pattern)?, quote_word(&replacement)?);
    for path in paths {
        line.push(' ');
        line.push_str(&quote_word(path)?);
    }
    Some(line)
}

/// Split an `s/pat/rep/g` script, or refuse it when the two engines would read
/// it differently.
fn sed_substitution(script: &str) -> Option<(String, String)> {
    let mut chars = script.chars();
    if chars.next()? != 's' {
        return None;
    }
    let delimiter = chars.next()?;
    if delimiter.is_alphanumeric() || delimiter.is_whitespace() || delimiter == '\\' {
        return None;
    }
    let rest: String = chars.collect();
    let mut parts = rest.split(delimiter);
    let pattern = parts.next()?;
    let replacement = parts.next()?;
    if parts.next()? != "g" || parts.next().is_some() {
        return None;
    }
    // An empty pattern reuses sed's previous regex; the Spore would match the
    // empty string everywhere and rewrite the file into nonsense.
    if pattern.is_empty() {
        return None;
    }
    const DIFFERS: &[char] = &['\\', '+', '?', '|', '(', ')', '{', '}'];
    if pattern.chars().any(|ch| DIFFERS.contains(&ch))
        || replacement.chars().any(|ch| DIFFERS.contains(&ch))
    {
        return None;
    }
    if replacement.contains(['&', '$']) {
        return None;
    }
    Some((pattern.to_string(), replacement.to_string()))
}

/// Single-quote a word for the Spore's tokenizer, or refuse when it holds a
/// quote the translation would have to escape.
fn quote_word(word: &str) -> Option<String> {
    if word.contains(['\'', '\n']) {
        return None;
    }
    Some(format!("'{word}'"))
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
    // An argument that was quoted counts even when it is empty: BSD sed is
    // invoked as `sed -i '' 's/a/b/' f`, and dropping that word turns the
    // script into a backup suffix and the file into a command.
    let mut quoted = false;
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
                '\'' | '"' => {
                    quote = Some(c);
                    quoted = true;
                }
                c if c.is_whitespace() => {
                    if !buffer.is_empty() || quoted {
                        out.push(std::mem::take(&mut buffer));
                    }
                    quoted = false;
                }
                _ => buffer.push(c),
            },
        }
    }
    if !buffer.is_empty() || quoted {
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
        // BSD sed's empty backup suffix is an argument, not nothing.
        assert_eq!(
            split_words("sed -i '' 's/a/b/' f"),
            vec!["sed", "-i", "", "s/a/b/", "f"]
        );
        assert_eq!(split_words("echo '' end"), vec!["echo", "", "end"]);
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

    #[test]
    fn diff_and_json_are_spore_owned() {
        // Both are on the host allowlist, so the routing test is that the Spore
        // still wins: the overlay copy would be a second, unindexed answer.
        assert!(host_allowed("diff"));
        assert!(spore_owned("diff"));
        assert!(spore_owned("json"));
        // Shell builtins that only reached the host list by accident.
        assert!(host_allowed("test"));
        assert!(spore_owned("test"));
        // Everything else keeps running as the host program.
        assert!(!spore_owned("git"));
        assert!(!spore_owned("rg"));
    }

    #[test]
    fn a_plain_sed_substitution_becomes_sub() {
        let route = |line: &str| {
            let words = split_words(line);
            spore_line(line, &words)
        };
        assert_eq!(
            route("sed -i 's/old name/new name/g' src/main.rs").as_deref(),
            Some("sub 'old name' 'new name' 'src/main.rs'")
        );
        // `-e` and the BSD empty backup suffix mean the same thing.
        assert_eq!(
            route("sed -i -e 's/^  //g' a.txt").as_deref(),
            Some("sub '^  ' '' 'a.txt'")
        );
        assert_eq!(
            route("sed -i '' 's/#/--/g' a.txt b.txt").as_deref(),
            Some("sub '#' '--' 'a.txt' 'b.txt'")
        );
        // Another delimiter, because patterns with slashes need one anyway.
        assert_eq!(
            route("sed -i 's|a/b|c|g' f").as_deref(),
            Some("sub 'a/b' 'c' 'f'")
        );
    }

    #[test]
    fn sed_forms_that_would_differ_stay_on_the_host() {
        let route = |line: &str| {
            let words = split_words(line);
            spore_line(line, &words)
        };
        // No `g`: sed replaces the first match per line, `sub` replaces all.
        assert_eq!(route("sed -i 's/a/b/' f"), None);
        // BRE groups and ERE operators read differently in the two engines.
        assert_eq!(route(r"sed -i 's/\(a\)/[\1]/g' f"), None);
        assert_eq!(route("sed -i 's/a+/b/g' f"), None);
        assert_eq!(route("sed -i 's/a|b/c/g' f"), None);
        assert_eq!(route(r"sed -i 's/\d/0/g' f"), None);
        // sed's `&` and `$n` are not the Spore's `$n`.
        assert_eq!(route("sed -i 's/a/[&]/g' f"), None);
        assert_eq!(route("sed -i 's/(a)/$1/g' f"), None);
        // A backup suffix writes a file we would not.
        assert_eq!(route("sed -i.bak 's/a/b/g' f"), None);
        // Two scripts, or no script at all.
        assert_eq!(route("sed -i -e 's/a/b/g' -e 's/c/d/g' f"), None);
        assert_eq!(route("sed -i f"), None);
        // Not in place: `sed 's/a/b/g' f` prints, it does not write.
        assert_eq!(route("sed 's/a/b/g' f"), None);
        // A quote the translation would have to escape.
        assert_eq!(route("sed -i \"s/a/it's/g\" f"), None);
    }

    #[test]
    fn sed_substitution_keeps_only_the_shared_subset() {
        let sub = sed_substitution;
        assert_eq!(sub("s/a/b/g"), Some(("a".into(), "b".into())));
        assert_eq!(sub("s/^  //g"), Some(("^  ".into(), "".into())));
        assert_eq!(sub("s/a$/b/g"), Some(("a$".into(), "b".into())));
        assert_eq!(sub("s/a.b/c/g"), Some(("a.b".into(), "c".into())));
        // Character classes and `*` mean the same thing in both engines.
        assert_eq!(sub("s/a[0-9]*/N/g"), Some(("a[0-9]*".into(), "N".into())));
        // `+` is literal in a BRE and an operator in the Spore's regex.
        assert_eq!(sub("s/[0-9]+/N/g"), None);
        assert_eq!(sub("s/a/b/gg"), None);
        // Empty pattern reuses sed's last regex; the Spore would match
        // everywhere.
        assert_eq!(sub("s//x/g"), None);
        assert_eq!(sub("y/a/b/g"), None);
    }
}
