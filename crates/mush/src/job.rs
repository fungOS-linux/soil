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

/// Patience before a command is left running in the background.
pub const DEFAULT_BACKGROUND: Duration = Duration::from_secs(10 * 60);
/// Never background sooner than this; a cold start alone can take seconds.
pub const MIN_BACKGROUND: Duration = Duration::from_secs(60);
/// Never wait longer than this before handing the caller back its life.
pub const MAX_BACKGROUND: Duration = Duration::from_secs(20 * 60);
/// Samples needed before the threshold adapts to observed durations.
const MIN_SAMPLES: usize = 3;
/// Samples kept per command.
const MAX_SAMPLES: usize = 16;

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
    sender: std::sync::mpsc::Sender<(String, String, bool, Option<Duration>)>,
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
    pub fn start(runtime: Runtime, root: PathBuf, default_threshold: Option<Duration>) -> Self {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner::default()),
            changed: Condvar::new(),
        });
        let (sender, receiver) = std::sync::mpsc::channel::<(String, String, bool, Option<Duration>)>();
        let worker_shared = Arc::clone(&shared);
        let cancel = runtime.cancel_flag();
        let engine = runtime.engine_handle();
        let control = crate::shell::Control::new(Arc::clone(&cancel)).with_budget(runtime.budget());
        let worker_control = control.clone();
        let worker_control_budget = runtime.budget();
        let mut runtime = runtime;
        let worker = thread::spawn(move || {
            while let Ok((id, command, shell, budget)) = receiver.recv() {
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
                let executed = run_command(&mut runtime, &root, &command, shell, &control);
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
        let after = threshold
            .or(self.default_threshold)
            .unwrap_or_else(|| self.suggest(command));
        let after = after.clamp(MIN_BACKGROUND, MAX_BACKGROUND);
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
        if self.sender.send((id.clone(), command.to_string(), shell, budget)).is_err() {
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
        let word = command
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let inner = self.lock();
        let Some(samples) = inner.durations.get(&word) else {
            return DEFAULT_BACKGROUND;
        };
        if samples.len() < MIN_SAMPLES {
            return DEFAULT_BACKGROUND;
        }
        let count = samples.len() as f64;
        let mean = samples.iter().map(|value| *value as f64).sum::<f64>() / count;
        let variance = samples
            .iter()
            .map(|value| (*value as f64 - mean).powi(2))
            .sum::<f64>()
            / count;
        let threshold = mean + 3.0 * variance.sqrt();
        let millis = threshold.clamp(
            MIN_BACKGROUND.as_millis() as f64,
            MAX_BACKGROUND.as_millis() as f64,
        );
        Duration::from_millis(millis as u64)
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
            let word = command
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            let samples = inner.durations.entry(word).or_default();
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
    command: &str,
    shell: bool,
    control: &crate::shell::Control,
) -> Executed {
    if shell {
        return match crate::shell::run_line_captured(runtime, root, command, control) {
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
