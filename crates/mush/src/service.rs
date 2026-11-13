//! One-request-per-connection Unix socket service for `mush --serve`.
//!
//! This is deliberately small. The service account owns the socket and VFS
//! root; the socket is 0600 and every accepted peer is checked against the
//! daemon's effective UID. Authentication above that boundary belongs to the
//! signed Mycelium/Soil principal, not this check.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::job::{Jobs, Outcome};

pub fn serve(socket: &Path, jobs: std::sync::Arc<Jobs>, idle: Option<Duration>) -> Result<()> {
    prepare_socket(socket)?;
    let listener = UnixListener::bind(socket)
        .with_context(|| format!("bind Mush service socket {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod Mush service socket {}", socket.display()))?;
    eprintln!("mush: serving on {}", socket.display());
    // A daemon nobody talks to is a leaked process and a stale socket. Idle
    // shutdown is what makes starting one per VFS root safe.
    listener.set_nonblocking(true)?;
    let mut idle_since = std::time::Instant::now();
    let _cleanup = RemoveOnDrop(socket.to_path_buf());
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                // One connection at a time would make `cancel` unreachable
                // while the command it is meant to stop is running. Each
                // request gets its own thread; the command engine still
                // serialises the work itself.
                let jobs = std::sync::Arc::clone(&jobs);
                std::thread::spawn(move || {
                    if let Err(error) = handle(stream, &jobs) {
                        eprintln!("mush: request: {error:#}");
                    }
                });
                idle_since = std::time::Instant::now();
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(idle) = idle
                    && idle_since.elapsed() >= idle
                {
                    eprintln!("mush: idle for {}s, stopping", idle.as_secs());
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                eprintln!("mush: accept: {error}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// The socket file belongs to the process that bound it; a daemon that exits
/// must not leave a path that looks alive to the next caller.
struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn prepare_socket(socket: &Path) -> Result<()> {
    if socket.exists() {
        bail!(
            "Mush service socket {} already exists; refusing to replace it",
            socket.display()
        );
    }
    if let Some(parent) = socket.parent() {
        let existed = parent.exists();
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        if !existed {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod {}", parent.display()))?;
        }
    }
    Ok(())
}

fn handle(stream: UnixStream, jobs: &Jobs) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    if !same_uid(&stream)? {
        let mut stream = stream;
        let _ = stream.write_all(b"{\"ok\":false,\"error\":\"peer uid denied\"}\n");
        bail!("peer uid does not match service uid");
    }
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        bail!("empty request");
    }
    let request: Value = serde_json::from_str(&line).context("parse request JSON")?;
    let kind = request
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("exec");
    let response = match kind {
        "cancel" => {
            let id = request
                .get("job")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match jobs.cancel(id) {
                Ok(response) => response,
                Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
            }
        }
        "jobs" => {
            let mut listing = jobs.list();
            listing["ok"] = json!(true);
            listing
        }
        "job" => {
            let id = request
                .get("job")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match jobs.snapshot(id) {
                Some(mut snapshot) => {
                    snapshot["ok"] = json!(true);
                    snapshot
                }
                None => json!({"ok": false, "error": format!("no job {id:?}")}),
            }
        }
        _ => {
            // A streaming caller watches the command instead of collecting it.
            // Everything else about the request is the same, so this branch sits
            // in front of the one-shot path rather than beside it.
            if request.get("stream").and_then(Value::as_bool) == Some(true) {
                let Some(command) = request.get("command").and_then(Value::as_str) else {
                    write_response(
                        stream,
                        &json!({"ok": false, "error": "request needs a string `command`"}),
                    )?;
                    return Ok(());
                };
                let shell = kind == "shell";
                let budget = request
                    .get("timeout_secs")
                    .and_then(Value::as_u64)
                    .map(Duration::from_secs);
                let sink = SocketOutput {
                    stream: stream.try_clone()?,
                    closed: false,
                };
                let outcome = jobs.submit_streaming(command, shell, budget, Box::new(sink))?;
                let frame = match outcome {
                    Outcome::Completed(executed) => json!({
                        "done": true,
                        "ok": true,
                        "status": executed.status,
                        "state": executed.state.as_str(),
                    }),
                    Outcome::Background { job, .. } => json!({
                        "done": true,
                        "ok": true,
                        "status": 0,
                        "state": "background",
                        "job": job,
                    }),
                };
                write_response(stream, &frame)?;
                return Ok(());
            }
            let Some(command) = request.get("command").and_then(Value::as_str) else {
                write_response(
                    stream,
                    &json!({"ok": false, "error": "request needs a string `command`"}),
                )?;
                return Ok(());
            };
            let threshold = request
                .get("background_after_secs")
                .and_then(Value::as_u64)
                .map(Duration::from_secs);
            let shell = kind == "shell";
            // A caller's own deadline, so a long build one client asked for is
            // not inherited by the next short command.
            let budget = request
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .map(Duration::from_secs);
            match jobs.submit(command, threshold, shell, budget)? {
                Outcome::Completed(executed) => json!({
                    "ok": true,
                    "output": executed.output,
                    "stderr": executed.stderr,
                    "exit": executed.exit,
                    "status": executed.status,
                    "state": executed.state.as_str(),
                    "blocking": executed.state.blocking(),
                }),
                Outcome::Background { job, after } => json!({
                    "ok": true,
                    "state": "background",
                    "blocking": false,
                    "job": job,
                    "after_secs": after.as_secs(),
                    "hint": format!(
                        "command still running after {}s; check it with `mush job {job}` or list with `mush jobs`",
                        after.as_secs()
                    ),
                }),
            }
        }
    };
    write_response(stream, &response)?;
    Ok(())
}

/// A live sink over the request socket.
///
/// Frames are newline-delimited JSON so a reader never has to buffer a whole
/// build to show its first line: `{"out": ...}`, `{"err": ...}`, then a final
/// `{"done": true, "status": ...}`.
struct SocketOutput {
    stream: UnixStream,
    closed: bool,
}

impl SocketOutput {
    fn send(&mut self, frame: Value) {
        if self.closed {
            return;
        }
        let mut text = frame.to_string();
        text.push('\n');
        if self.stream.write_all(text.as_bytes()).is_err() || self.stream.flush().is_err() {
            // The reader went away (a cancelled tool call). Keep running so the
            // job record still ends up correct; just stop writing.
            self.closed = true;
        }
    }
}

impl crate::shell::Output for SocketOutput {
    fn stdout(&mut self, text: &str) {
        self.send(json!({ "out": text }));
    }
    fn stderr(&mut self, text: &str) {
        self.send(json!({ "err": text }));
    }
    fn inherits_stdio(&self) -> bool {
        false
    }
}

/// Send one streaming request and relay frames until the daemon says done.
fn request_stream(socket: &Path, request: &Value, wait: Duration) -> Option<i32> {
    let stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(wait)).ok()?;
    let mut writer = stream.try_clone().ok()?;
    writer
        .write_all(serde_json::to_string(request).ok()?.as_bytes())
        .ok()?;
    writer.write_all(b"\n").ok()?;
    writer.flush().ok()?;
    let reader = BufReader::new(stream);
    let mut status = 0;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(text) = frame.get("out").and_then(Value::as_str) {
            print!("{text}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        if let Some(text) = frame.get("err").and_then(Value::as_str) {
            eprint!("{text}");
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }
        if let Some(text) = frame.get("output").and_then(Value::as_str) {
            print!("{text}");
        }
        if let Some(text) = frame.get("stderr").and_then(Value::as_str) {
            eprint!("{text}");
        }
        if let Some(error) = frame.get("error").and_then(Value::as_str) {
            eprintln!("mush: {error}");
            status = 1;
        }
        if let Some(code) = frame.get("status").and_then(Value::as_i64) {
            status = code as i32;
        }
        if frame.get("done").and_then(Value::as_bool) == Some(true) {
            break;
        }
    }
    Some(status)
}

/// What a client sends for one command line.
///
/// `budget` is the caller's deadline. It is also the background threshold unless
/// the caller asked for adaptive backgrounding: an agent that passes
/// `--timeout 900` as a safety net would otherwise wait the full 15 minutes
/// before learning that its command is stuck, which is the failure the job
/// queue exists to prevent.
fn request_body(line: &str, shell: bool, budget: Option<Duration>, adaptive: bool) -> Value {
    let mut request = json!({
        "kind": if shell { "shell" } else { "exec" },
        "command": line,
        "stream": true,
    });
    if let Some(budget) = budget {
        request["timeout_secs"] = json!(budget.as_secs());
        if !adaptive {
            request["background_after_secs"] = json!(budget.as_secs());
        }
    }
    request
}

fn write_response(mut stream: UnixStream, response: &Value) -> Result<()> {
    stream.write_all(serde_json::to_string(response)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn same_uid(stream: &UnixStream) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let mut uid = 0;
    let mut gid = 0;
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        bail!("getpeereid: {}", std::io::Error::last_os_error());
    }
    Ok(uid == unsafe { libc::geteuid() })
}

#[cfg(target_os = "linux")]
fn same_uid(stream: &UnixStream) -> Result<bool> {
    use std::{mem::size_of, os::fd::AsRawFd};
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut _ as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        bail!(
            "getsockopt(SO_PEERCRED): {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(credentials.uid == unsafe { libc::geteuid() })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn same_uid(_stream: &UnixStream) -> Result<bool> {
    bail!("peer UID checks are not implemented for this platform")
}

/// Where the daemon for `root` listens: the configured socket, or a stable
/// per-root path so two Desktops never share one command engine.
pub fn socket_for(configured: Option<&Path>, root: &Path) -> PathBuf {
    if let Some(path) = configured {
        return path.to_path_buf();
    }
    let dir = crate::config::expand_home("~/.local/share/fungos/mush/run");
    dir.join(format!(
        "{:016x}.sock",
        fnv1a(root.to_string_lossy().as_bytes())
    ))
}

/// FNV-1a, so the socket name for a root is the same on every start and in
/// every process without pulling in a hasher whose output can change.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Run `line` through the daemon for `root`.
///
/// `None` means no daemon answered and none could be started, so the caller
/// should run the command in-process; `Some(status)` is the command's own exit
/// status, already printed.
/// What a one-shot caller wants forwarded to the daemon.
pub struct Forward<'a> {
    pub line: &'a str,
    /// Run the line as a shell line (builtins, `&&`/`||`, host programs) rather
    /// than straight to the Spore.
    pub shell: bool,
    pub idle: Duration,
    /// The caller's deadline, when it set one.
    pub budget: Option<Duration>,
    /// Let the daemon choose the background threshold from the command's own
    /// timings instead of treating `budget` as the caller's patience.
    pub adaptive: bool,
}

pub fn forward(
    socket: &Path,
    root: &Path,
    desktop: Option<&str>,
    forward: Forward<'_>,
) -> Option<i32> {
    let Forward {
        line,
        shell,
        idle,
        budget,
        adaptive,
    } = forward;
    let request = request_body(line, shell, budget, adaptive);
    let wait = budget.unwrap_or(Duration::from_secs(120)) + Duration::from_secs(5);
    // Streamed, not one-shot: a build that runs for ten minutes has to show its
    // first line in the first second, and the caller's exit status still comes
    // from the daemon's own record of the command.
    let mut status = request_stream(socket, &request, wait);
    if status.is_none() {
        // A socket file with nothing behind it is what a killed daemon leaves,
        // and a cold daemon is the first command after an idle shutdown.
        let _ = std::fs::remove_file(socket);
        if !start(socket, root, desktop, idle, budget) {
            return None;
        }
        status = request_stream(socket, &request, wait);
    }
    status
}

/// One request/response exchange, or `None` when no daemon is listening.
pub(crate) fn request_once(
    socket: &Path,
    request: &Value,
    wait: Duration,
) -> Option<Value> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(wait)).ok()?;
    stream.set_write_timeout(Some(wait)).ok()?;
    stream
        .write_all(serde_json::to_string(request).ok()?.as_bytes())
        .ok()?;
    stream.write_all(b"\n").ok()?;
    stream.flush().ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// Start a daemon for `root` and wait until it accepts, or give up.
fn start(
    socket: &Path,
    root: &Path,
    desktop: Option<&str>,
    idle: Duration,
    budget: Option<Duration>,
) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut command = std::process::Command::new(exe);
    command
        .arg("--serve")
        .arg("--foreground")
        .arg("--socket")
        .arg(socket)
        .arg("--idle-timeout")
        .arg(idle.as_secs().to_string());
    if let Some(budget) = budget {
        command.arg("--timeout").arg(budget.as_secs().to_string());
    }
    match desktop {
        Some(id) => {
            command.arg("--desktop").arg(id);
        }
        None => {
            command.arg("--root").arg(root);
        }
    }
    // The daemon outlives the caller, so it must not hold the caller's
    // terminal, pipe, or session.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // A daemon in the caller's process group dies with the terminal that
    // started it (the hangup is sent to the foreground group), which would
    // make it a per-terminal cache instead of a per-root one.
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if command.spawn().is_err() {
        return false;
    }
    // The first start compiles the Spore; give it the same patience a cold
    // in-process run would have taken anyway.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_backgrounding_leaves_the_threshold_to_the_daemon() {
        let budget = Some(Duration::from_secs(900));
        let request = request_body("cargo build", true, budget, true);
        assert_eq!(request["kind"], json!("shell"));
        assert_eq!(request["timeout_secs"], json!(900));
        assert!(
            request.get("background_after_secs").is_none(),
            "the caller's deadline must not become the background threshold: {request}"
        );

        // Without the flag the old contract holds: a caller that asked to wait
        // is not handed a job id early.
        let request = request_body("cargo build", true, budget, false);
        assert_eq!(request["background_after_secs"], json!(900));

        // No deadline, no threshold either way.
        let request = request_body("git status", false, None, false);
        assert_eq!(request["kind"], json!("exec"));
        assert!(request.get("timeout_secs").is_none());
        assert!(request.get("background_after_secs").is_none());
    }
}
