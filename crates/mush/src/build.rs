//! The build plane, from mush's side.
//!
//! bob decides which lane a build runs in and what it may reuse; mush owns the
//! namespace it runs in. They meet at exactly one place — the host process mush
//! spawns for a build — and this module is that meeting point.
//!
//! Two rules keep the boundary honest:
//!
//! - **bob's answer is data.** `~/.config/sccache/env` is written for a shell:
//!   it has to be sourced, and part of it is shell aliases. A spawned child sees
//!   neither, so mush asks `fpl bob env` for the same facts as `KEY=VALUE`.
//!   The alternatives — parsing that file here, or copying its values into this
//!   crate — are two more places for bob and mush to disagree about the build.
//! - **The identity is mush's.** `BOB_AGENT_KEY` names the session mush is
//!   serving, not whatever the daemon's shell exported, so one agent's build
//!   never shares a package-cache lock with another's.
//!
//! Fail-open throughout: no bob, a bob too old to know `env`, or a failed probe
//! all leave the child with exactly the environment it would have had. A broken
//! build plane must not become a broken shell.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Programs whose children are a build. Asking bob for every host command would
/// pay its cost — a per-agent `CARGO_HOME` clones gigabytes on first use — for
/// `ls`. A build is still one entry point; its build scripts inherit the rest.
const BUILD_PROGRAMS: &[&str] = &[
    "cargo", "rustc", "cc", "c++", "gcc", "g++", "clang", "clang++", "make", "cmake", "ninja",
    "zig", "go", "npm", "pnpm", "yarn", "bun",
];

/// How long bob's answer is trusted. It only changes when someone re-runs
/// `fpl bob setup`, but a long-lived daemon should notice that without a
/// restart, and one probe a minute is nothing next to a build.
const TTL: Duration = Duration::from_secs(60);

/// What bob says a build needs, minus the layout details a child does not need.
#[derive(Clone, Default)]
pub struct Build {
    vars: Vec<(String, String)>,
    cargo: Option<PathBuf>,
    bin_dir: Option<PathBuf>,
    /// The session the build belongs to. Stable across the throwaway overlays
    /// mush materializes, which is what lets a remote builder keep its target.
    agent: String,
}

impl Build {
    /// bob's environment for `program`, or `None` when bob does not apply.
    pub fn prepare(program: &str, agent: &str) -> Option<Build> {
        if !BUILD_PROGRAMS.contains(&program) {
            return None;
        }
        resolve(agent).map(|mut build| {
            build.agent = agent.to_string();
            build
        })
    }

    /// The program to spawn. `cargo` becomes bob's shim, which is what keeps
    /// lane routing part of every build mush runs rather than a habit nobody
    /// can enforce.
    pub fn program(&self, program: &str) -> PathBuf {
        match (program, &self.cargo) {
            ("cargo", Some(cargo)) => cargo.clone(),
            _ => PathBuf::from(program),
        }
    }

    /// Put bob's answer on the child, plus the one fact bob cannot know: where
    /// this run's overlay is.
    pub fn apply(&self, command: &mut Command, overlay: &Path) {
        for (key, value) in &self.vars {
            command.env(key, value);
        }
        if let Some(bin) = &self.bin_dir {
            // bob's compilers are reached through PATH by build scripts and
            // nested cargo; the exported `CC`/`CXX` only cover the first level.
            let mut path = OsString::from(bin);
            path.push(":");
            path.push(current_env(command, "PATH").unwrap_or_default());
            command.env("PATH", path);
        }
        // A remote build lives in a named workspace on the builder. mush
        // materializes a fresh overlay per command, so a name derived from the
        // working directory would be new every time and the remote target
        // would never be warm. The session is the stable thing mush knows.
        if !self.agent.is_empty() && current_env(command, "BOB_REMOTE_SESSION").is_none() {
            command.env("BOB_REMOTE_SESSION", &self.agent);
        }
        // The overlay is a fresh path on every run, so rustc would hash a
        // different source prefix each time and the cache would never hit.
        // Naming this run's overlay as a basedir makes that prefix a constant.
        let mut basedirs = current_env(command, "SCCACHE_BASEDIRS")
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !basedirs.split(':').any(|dir| Path::new(dir) == overlay) {
            if !basedirs.is_empty() {
                basedirs.push(':');
            }
            basedirs.push_str(&overlay.to_string_lossy());
            command.env("SCCACHE_BASEDIRS", basedirs);
        }
    }
}

/// What `mush doctor` should say about the next build.
pub fn describe(agent: &str) -> String {
    match resolve(agent) {
        Some(build) => {
            let find = |name: &str| {
                build
                    .vars
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.as_str())
                    .unwrap_or("(none)")
            };
            format!(
                "bob agent={agent} cargo-home={} target={} wrapper={}",
                find("CARGO_HOME"),
                find("CARGO_TARGET_DIR"),
                find("RUSTC_WRAPPER"),
            )
        }
        None => "bob: not answering (builds run uncached, on this machine)".into(),
    }
}

struct Cached {
    at: Instant,
    build: Option<Build>,
}

fn cache() -> &'static Mutex<Option<(String, Cached)>> {
    static CACHE: Mutex<Option<(String, Cached)>> = Mutex::new(None);
    &CACHE
}

/// One bob answer per agent, refreshed after [`TTL`].
fn resolve(agent: &str) -> Option<Build> {
    // A poisoned lock must not take a build down with it: fall through to the
    // uncached path and let the probe decide.
    if let Ok(slot) = cache().lock()
        && let Some((key, entry)) = slot.as_ref()
        && key == agent
        && entry.at.elapsed() < TTL
    {
        return entry.build.clone();
    }
    let build = ask(agent);
    if let Ok(mut slot) = cache().lock() {
        *slot = Some((
            agent.to_string(),
            Cached {
                at: Instant::now(),
                build: build.clone(),
            },
        ));
    }
    build
}

/// Ask bob. Two spellings, because bob ships both as a subcommand of `fpl` and
/// as its own binary, and a machine may have only one of them.
fn ask(agent: &str) -> Option<Build> {
    let key = format!("--key={agent}");
    let attempts: [&[&str]; 2] = [
        &["fpl", "bob", "env", key.as_str(), "--target-dir"],
        &["fpl-bob", "env", key.as_str(), "--target-dir"],
    ];
    for argv in attempts {
        let Ok(out) = Command::new(argv[0]).args(&argv[1..]).output() else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        if let Some(build) = parse(&String::from_utf8_lossy(&out.stdout)) {
            return Some(build);
        }
    }
    None
}

/// bob's `KEY=VALUE` lines. `BOB_CARGO` and `BOB_BIN_DIR` describe bob's own
/// layout, so they leave the environment: a child does not need to know where
/// the shim lives, only that `cargo` is it.
fn parse(text: &str) -> Option<Build> {
    let mut build = Build::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.is_empty() || value.is_empty() {
            continue;
        }
        match key {
            "BOB_CARGO" => build.cargo = Some(PathBuf::from(value)),
            "BOB_BIN_DIR" => build.bin_dir = Some(PathBuf::from(value)),
            _ => build.vars.push((key.to_string(), value.to_string())),
        }
    }
    // No `CARGO_HOME` means this is not bob's answer — an older bob, or one
    // that was never set up. Half-applying that is worse than not applying:
    // the feature is the per-agent home.
    let home = build.vars.iter().any(|(key, _)| key == "CARGO_HOME");
    home.then_some(build)
}

/// A variable's value as the child will see it: what the command already set,
/// otherwise what this process would pass down.
fn current_env(command: &Command, key: &str) -> Option<OsString> {
    command
        .get_envs()
        .find(|(name, _)| *name == key)
        .and_then(|(_, value)| value.map(|value| value.to_owned()))
        .or_else(|| std::env::var_os(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_takes_bobs_layout_out_of_the_environment() {
        let build = parse(
            "BOB_AGENT_KEY=a\nCARGO_HOME=/home/a/cargo\nBOB_CARGO=/bob/bob-cargo\nBOB_BIN_DIR=/bob\n",
        )
        .expect("an answer with a CARGO_HOME is bob's");
        assert_eq!(build.program("cargo"), PathBuf::from("/bob/bob-cargo"));
        assert_eq!(build.program("rustc"), PathBuf::from("rustc"));
        assert!(build.vars.iter().any(|(key, _)| key == "CARGO_HOME"));
        assert!(!build.vars.iter().any(|(key, _)| key == "BOB_CARGO"));
        assert!(!build.vars.iter().any(|(key, _)| key == "BOB_BIN_DIR"));
    }

    #[test]
    fn an_answer_without_a_cargo_home_is_not_bobs() {
        assert!(parse("BOB_AGENT_KEY=a\n").is_none());
        assert!(parse("").is_none());
        assert!(parse("not even key value\n").is_none());
    }

    #[test]
    fn apply_prepends_bobs_bin_dir_to_path() {
        let build = parse("CARGO_HOME=/h\nBOB_BIN_DIR=/bob\nBOB_CARGO=/bob/bob-cargo\n").unwrap();
        let mut command = Command::new("cargo");
        command.env("PATH", "/usr/bin");
        build.apply(&mut command, Path::new("/overlay"));
        assert_eq!(
            current_env(&command, "PATH"),
            Some(OsString::from("/bob:/usr/bin"))
        );
    }

    #[test]
    fn apply_adds_the_overlay_as_a_basedir_once() {
        let build = parse(
            "CARGO_HOME=/h\nSCCACHE_BASEDIRS=/src:/tmp\nBOB_BIN_DIR=/bob\nBOB_CARGO=/bob/bob-cargo\n",
        )
        .unwrap();
        let mut command = Command::new("cargo");
        build.apply(&mut command, Path::new("/tmp"));
        build.apply(&mut command, Path::new("/overlay"));
        build.apply(&mut command, Path::new("/overlay"));
        assert_eq!(
            current_env(&command, "SCCACHE_BASEDIRS"),
            Some(OsString::from("/src:/tmp:/overlay"))
        );
    }

    #[test]
    fn apply_names_the_remote_session() {
        let mut build = parse("CARGO_HOME=/h\nBOB_BIN_DIR=/bob\nBOB_CARGO=/bob/bob-cargo\n").unwrap();
        build.agent = "root-abc".into();
        let mut command = Command::new("cargo");
        build.apply(&mut command, Path::new("/overlay"));
        assert_eq!(
            current_env(&command, "BOB_REMOTE_SESSION"),
            Some(OsString::from("root-abc"))
        );
        // An explicit session is the caller's to choose.
        let mut command = Command::new("cargo");
        command.env("BOB_REMOTE_SESSION", "mine");
        build.apply(&mut command, Path::new("/overlay"));
        assert_eq!(
            current_env(&command, "BOB_REMOTE_SESSION"),
            Some(OsString::from("mine"))
        );
    }

    #[test]
    fn a_command_that_cannot_build_gets_no_environment() {
        assert!(Build::prepare("ls", "agent").is_none());
        assert!(Build::prepare("git", "agent").is_none());
    }

    #[test]
    fn describe_says_so_when_bob_is_absent() {
        // `describe` never panics; with bob present it names the paths.
        let line = describe("test-agent");
        assert!(line.starts_with("bob"), "{line}");
    }
}
