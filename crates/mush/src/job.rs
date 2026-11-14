//! Jobs for commands that outlive their caller's patience.
//!
//! `deadline.rs` stops a command. This stops the *wait*. A command that runs
//! past its soft threshold keeps running on a worker while the caller gets an
//! id to come back to, because killing a useful long build is its own failure
//! and blocking a caller forever is the failure we are fixing.
//!
//! The threshold adapts: a command that usually takes 5s should not get the
//! same patience as one that usually takes 8 minutes. Once a command has a few
//! recorded durations, its threshold is the mean plus three standard
//! deviations, clamped to a sane band.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{
    Runtime,
    outcome::{Executed, State},
};

/// Patience for a command that is expected to be slow and has no history yet:
/// a cold build, a network fetch, a push.
pub const PATIENT_BACKGROUND: Duration = Duration::from_secs(10 * 60);
/// Patience for anything else with no history. An agent stuck behind a `find`
/// that never returns loses a session; 30 seconds is long enough for an honest
/// command to finish and short enough that a stuck one comes back as a job.
pub const UNSEEN_BACKGROUND: Duration = Duration::from_secs(30);
/// The shortest patience an adapted command can get; below this a fast command
/// would be handed back before it had a chance to finish.
pub const MIN_BACKGROUND: Duration = Duration::from_secs(2);
/// Never wait longer than this before handing the caller back its life.
pub const MAX_BACKGROUND: Duration = Duration::from_secs(20 * 60);
/// Samples needed before the threshold adapts: one number has no deviation.
const MIN_SAMPLES: usize = 2;

/// Invocations that are slow by nature even without a sample.
const PATIENT: &[&str] = &[
    "cargo",
    "rustc",
    "cc",
    "clang",
    "c++",
    "make",
    "cmake",
    "ninja",
    "pkg-config",
    "rustup",
    "npm",
    "pnpm",
    "yarn",
    "bun install",
    "docker",
    "podman",
    "git push",
    "git pull",
    "git fetch",
    "git clone",
    "git merge",
    "git rebase",
    "git submodule",
    "gh",
];

fn patient(key: &str) -> bool {
    PATIENT
        .iter()
        .any(|prefix| key == *prefix || (key.starts_with(prefix) && key[prefix.len()..].starts_with(' ')))
}

/// The bucket a command's durations are recorded under.
///
/// The first word, except for tools whose verbs have very different costs:
/// `git status` and `git push` must not share a threshold, or a fast status
/// makes every push a background job.
fn sample_key(command: &str) -> String {
    const SUBCOMMANDED: &[&str] = &["git", "gh", "cargo", "rustup"];
    let mut words = command.split_whitespace();
    let Some(first) = words.next() else {
        return String::new();
    };
    if SUBCOMMANDED.contains(&first)
        && let Some(second) = words.next()
        && !second.starts_with('-')
    {
        return format!("{first} {second}");
    }
    first.to_string()
}

/// Patience for `key`, given the durations recorded for it so far.
///
/// Mean plus one standard deviation, floored and capped: a command that
/// usually takes 200ms gets the floor, one that usually takes 40s gets its own
/// spread, and a command that has not been timed twice yet borrows a default
/// chosen by what it is.
fn threshold_for(key: &str, samples: &[u64]) -> Duration {
    if samples.len() < MIN_SAMPLES {
        return if patient(key) {
            PATIENT_BACKGROUND
        } else {
            UNSEEN_BACKGROUND
        };
    }
    let count = samples.len() as f64;
    let mean = samples.iter().map(|value| *value as f64).sum::<f64>() / count;
    let variance = samples
        .iter()
        .map(|value| (*value as f64 - mean).powi(2))
        .sum::<f64>()
        / count;
    let millis = (mean + variance.sqrt()).clamp(
        MIN_BACKGROUND.as_millis() as f64,
        MAX_BACKGROUND.as_millis() as f64,
    );
    Duration::from_millis(millis as u64)
}
/// Samples kept per command.
const MAX_SAMPLES: usize = 16;

/// How long a streaming caller waits without its own deadline. Long enough for
/// a build, short enough that a wedged command eventually returns its status.
const MAX_STREAM_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// One queued command: what to run, and where to send output while it runs.
pub struct Task {
    id: String,
    command: String,
    shell: bool,
    budget: Option<Duration>,
    /// A caller that streamed the command is watching it now, so the worker
    /// copies output to it instead of waiting for the job record.
    live: Option<Box<dyn crate::shell::Output + Send>>,
}

pub enum Outcome {
    /// The command finished before the threshold.
    Completed(Executed),
    /// The command is still running; this is how to find it again.
    Background { job: String, after: Duration },
}

struct Job {
    command: String,
    started_ms: u64,
    finished: Option<Executed>,
    /// Set by `cancel`; a job that has not started yet is dropped instead of
    /// run, and a running job is interrupted through the engine epoch.
    cancel_requested: bool,
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    durations: HashMap<String, Vec<u64>>,
    seq: u64,
    /// The job the worker is executing right now.
    running: Option<String>,
}

struct Shared {
    inner: Mutex<Inner>,
    changed: Condvar,
}

pub struct Jobs {
    shared: Arc<Shared>,
    sender: std::sync::mpsc::Sender<Task>,
    /// The worker's cancel flag and engine, so a caller can stop a command
    /// that is already running instead of waiting for it to finish.
    cancel: Arc<AtomicBool>,
    engine: wasmtime::Engine,
    /// The running command's handle, so cancel can kill a host program too.
    control: crate::shell::Control,
    /// Threshold from `--background-after`, overriding the adaptive default.
    default_threshold: Option<Duration>,
    _worker: thread::JoinHandle<()>,
}

impl Jobs {
    /// Move `runtime` onto a worker thread and return a handle to it.
    ///
    /// One worker, one engine: Mush serialises commands today, and a single
    /// owner keeps the VFS and records view consistent without locking them.
    pub fn start(
        runtime: Runtime,
        root: PathBuf,
        default_threshold: Option<Duration>,
        store: Option<PathBuf>,
    ) -> Self {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner::default()),
            changed: Condvar::new(),
        });
        let (sender, receiver) = std::sync::mpsc::channel::<Task>();
        let worker_shared = Arc::clone(&shared);
        let cancel = runtime.cancel_flag();
        let engine = runtime.engine_handle();
        let control = crate::shell::Control::new(Arc::clone(&cancel)).with_budget(runtime.budget());
        let worker_control = control.clone();
        let worker_control_budget = runtime.budget();
        let mut runtime = runtime;
        let worker = thread::spawn(move || {
            while let Ok(Task { id, command, shell, budget, live }) = receiver.recv() {
                // Cancelled while it sat in the queue: never run it.
                if {
                    let mut inner = worker_shared.inner.lock().expect("job lock");
                    let requested = inner
                        .jobs
                        .get(&id)
                        .map(|job| job.cancel_requested)
                        .unwrap_or(false);
                    if !requested {
                        inner.running = Some(id.clone());
                    }
                    requested
                } {
                    worker_shared.finish(&id, command, Executed::cancelled());
                    continue;
                }
                let mut control = worker_control.clone();
                control.set_budget(budget.or(worker_control_budget));
                let executed = run_command(
                    &mut runtime,
                    &root,
                    store.as_deref(),
                    &command,
                    shell,
                    &control,
                    live,
                );
                worker_shared.inner.lock().expect("job lock").running = None;
                worker_shared.finish(&id, command, executed);
            }
        });
        Self {
            shared,
            sender,
            cancel,
            engine,
            control,
            default_threshold,
            _worker: worker,
        }
    }

    /// Stop a job: queued work is dropped, running work is trapped.
    ///
    /// The trap reuses the epoch machinery the deadline uses, so a cancelled
    /// command returns as a result (`state: cancelled`) instead of surfacing a
    /// Wasm trap the caller cannot act on.
    pub fn cancel(&self, id: &str) -> Result<Value> {
        let mut inner = self.lock();
        let Some(job) = inner.jobs.get_mut(id) else {
            bail!("no job {id:?}");
        };
        job.cancel_requested = true;
        let running = inner.running.as_deref() == Some(id);
        if running {
            self.cancel.store(true, Ordering::SeqCst);
            // A guest traps on the epoch bump; a host program has to be killed.
            self.engine.increment_epoch();
            self.control.kill();
        }
        Ok(json!({
            "ok": true,
            "job": id,
            "cancelled": true,
            "state": if running { "running" } else { "queued" },
        }))
    }

    /// Run `command`, waiting at most `threshold` before handing back an id.
    /// Queue `command`. `shell` runs it as a shell line (builtins, `&&`/`||`,
    /// allowlisted host programs); otherwise it goes straight to the Spore.
    pub fn submit(
        &self,
        command: &str,
        threshold: Option<Duration>,
        shell: bool,
        budget: Option<Duration>,
    ) -> Result<Outcome> {
        self.submit_with(command, threshold, shell, budget, None)
    }

    /// Same queue, but the caller watches it: output is streamed while the
    /// command runs and the caller waits for the exit status instead of being
    /// handed a job id. This is the `bash -c` path, where a caller that streams
    /// has already decided to wait.
    pub fn submit_streaming(
        &self,
        command: &str,
        shell: bool,
        budget: Option<Duration>,
        live: Box<dyn crate::shell::Output + Send>,
    ) -> Result<Outcome> {
        self.submit_with(command, None, shell, budget, Some(live))
    }

    fn submit_with(
        &self,
        command: &str,
        threshold: Option<Duration>,
        shell: bool,
        budget: Option<Duration>,
        live: Option<Box<dyn crate::shell::Output + Send>>,
    ) -> Result<Outcome> {
        let streaming = live.is_some();
        // A streaming caller is not asking to be handed a job id: its deadline
        // is how long it is willing to watch, so backgrounding would only take
        // the output away from it.
        let after = if streaming {
            budget.unwrap_or(MAX_STREAM_WAIT)
        } else {
            threshold
                .or(self.default_threshold)
                .unwrap_or_else(|| self.suggest(command))
        };
        let after = after.clamp(MIN_BACKGROUND, MAX_BACKGROUND.max(after));
        let id = {
            let mut inner = self.lock();
            inner.seq += 1;
            let id = format!("j{}", inner.seq);
            inner.jobs.insert(
                id.clone(),
                Job {
                    command: command.to_string(),
                    started_ms: now_ms(),
                    finished: None,
                    cancel_requested: false,
                },
            );
            id
        };
        if self
            .sender
            .send(Task {
                id: id.clone(),
                command: command.to_string(),
                shell,
                budget,
                live,
            })
            .is_err()
        {
            bail!("mush worker stopped; cannot run {command:?}");
        }

        let deadline = Instant::now() + after;
        let mut inner = self.lock();
        loop {
            if let Some(job) = inner.jobs.get(&id)
                && job.finished.is_some()
            {
                let job = inner.jobs.remove(&id).expect("job present");
                return Ok(Outcome::Completed(job.finished.expect("finished")));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(Outcome::Background { job: id, after });
            }
            let (guard, _) = self
                .shared
                .changed
                .wait_timeout(inner, deadline - now)
                .unwrap_or_else(|error| error.into_inner());
            inner = guard;
        }
    }

    /// One job as JSON, for `mush job ID` and the MCP status tool.
    pub fn snapshot(&self, id: &str) -> Option<Value> {
        let inner = self.lock();
        let job = inner.jobs.get(id)?;
        Some(match &job.finished {
            Some(executed) => json!({
                "job": id,
                "command": job.command,
                "state": "done",
                "status": executed.status,
                "stdout": executed.output,
                "stderr": executed.stderr,
            }),
            None => json!({
                "job": id,
                "command": job.command,
                "state": "running",
                "started_ms": job.started_ms,
                "hint": format!("still running; check again with `mush job {id}`"),
            }),
        })
    }

    pub fn list(&self) -> Value {
        let inner = self.lock();
        let mut jobs: Vec<Value> = inner
            .jobs
            .iter()
            .map(|(id, job)| {
                json!({
                    "job": id,
                    "command": job.command,
                    "state": if job.finished.is_some() { "done" } else { "running" },
                    "started_ms": job.started_ms,
                })
            })
            .collect();
        jobs.sort_by(|a, b| a["job"].as_str().cmp(&b["job"].as_str()));
        json!({ "jobs": jobs })
    }

    /// Patience for `command`, adapted from its recorded durations.
    pub fn suggest(&self, command: &str) -> Duration {
        let key = sample_key(command);
        let inner = self.lock();
        let samples = inner
            .durations
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or_default();
        threshold_for(&key, samples)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

impl Shared {
    fn finish(&self, id: &str, command: String, executed: Executed) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            let elapsed = started_ago(&inner, id);
            let key = sample_key(&command);
            let samples = inner.durations.entry(key).or_default();
            samples.push(elapsed);
            if samples.len() > MAX_SAMPLES {
                samples.remove(0);
            }
            if let Some(job) = inner.jobs.get_mut(id) {
                job.finished = Some(executed);
            }
        }
        self.changed.notify_all();
    }
}

/// Run every line of a command the way the CLI does, as one result.
fn run_command(
    runtime: &mut Runtime,
    root: &Path,
    store: Option<&Path>,
    command: &str,
    shell: bool,
    control: &crate::shell::Control,
    live: Option<Box<dyn crate::shell::Output + Send>>,
) -> Executed {
    if shell {
        // A streaming caller gets a tee: the text goes out as it is produced
        // and the job still ends up holding the same bytes.
        let captured = match live {
            Some(live) if shell => {
                let mut tee = crate::shell::Tee::new(live);
                match crate::shell::run_line_with(&mut tee, runtime, root, store, command, control) {
                    Ok(status) => (Ok((status, tee.captured.out, tee.captured.err))),
                    Err(error) => (Err(error)),
                }
            }
            _ => crate::shell::run_line_captured(runtime, root, store, command, control),
        };
        return match captured {
            Ok((status, output, stderr)) => Executed {
                output,
                stderr,
                exit: false,
                status,
                // A caller reading only `state` must see why the command
                // stopped, not a bare "done".
                state: match status {
                    crate::outcome::CANCELLED_STATUS => State::Cancelled,
                    crate::outcome::TIMEOUT_STATUS => State::Timeout,
                    _ => State::Done,
                },
            },
            Err(error) => Executed::done(String::new(), format!("mush: {error:#}\n"), false, 1),
        };
    }
    let mut output = String::new();
    let mut stderr = String::new();
    let mut status = 0;
    let mut state = State::Done;
    for line in command.lines() {
        match runtime.execute(line) {
            Ok(executed) => {
                output.push_str(&executed.output);
                stderr.push_str(&executed.stderr);
                status = executed.status;
                if executed.state != State::Done {
                    state = executed.state;
                }
                if executed.exit {
                    break;
                }
            }
            Err(error) => {
                stderr.push_str(&format!("mush: {error:#}\n"));
                status = 1;
                break;
            }
        }
    }
    Executed {
        output,
        stderr,
        exit: false,
        status,
        state,
    }
}

fn started_ago(inner: &Inner, id: &str) -> u64 {
    inner
        .jobs
        .get(id)
        .map(|job| (now_ms().saturating_sub(job.started_ms)) as u64)
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unseen_command_gets_short_patience_unless_it_is_expected_to_be_slow() {
        // The failure this fixes: an agent behind a `find` that never returns.
        assert_eq!(threshold_for("find", &[]), UNSEEN_BACKGROUND);
        assert_eq!(threshold_for("git status", &[]), UNSEEN_BACKGROUND);
        assert_eq!(threshold_for("rg", &[]), UNSEEN_BACKGROUND);
        // A cold build or a network fetch is allowed its time.
        assert_eq!(threshold_for("cargo build", &[]), PATIENT_BACKGROUND);
        assert_eq!(threshold_for("git push", &[]), PATIENT_BACKGROUND);
        assert_eq!(threshold_for("gh pr create", &[]), PATIENT_BACKGROUND);
        assert_eq!(threshold_for("make", &[]), PATIENT_BACKGROUND);
    }

    #[test]
    fn a_sampled_command_gets_its_own_mean_plus_one_sigma() {
        // Mean 1s, no deviation: the floor keeps it from being cut short.
        assert_eq!(threshold_for("find", &[1_000, 1_000]), MIN_BACKGROUND);
        // Mean 11s, sigma 1s.
        assert_eq!(
            threshold_for("git status", &[10_000, 12_000]),
            Duration::from_secs(12)
        );
        // One number is not a distribution.
        assert_eq!(threshold_for("git status", &[10_000]), UNSEEN_BACKGROUND);
        // A slow command is capped rather than handed a day.
        assert_eq!(
            threshold_for("cargo build", &[u64::MAX / 2, u64::MAX / 2]),
            MAX_BACKGROUND
        );
    }

    #[test]
    fn the_sample_key_keeps_git_verbs_apart() {
        // A fast `git status` must not teach the daemon that `git push` is
        // cheap: they are different buckets.
        assert_eq!(sample_key("git status --short"), "git status");
        assert_eq!(sample_key("git push origin main"), "git push");
        assert_eq!(sample_key("cargo build --release"), "cargo build");
        assert_eq!(sample_key("git --version"), "git");
        assert_eq!(sample_key("find / -name '*.rs'"), "find");
        assert_eq!(sample_key("   "), "");
    }
}
