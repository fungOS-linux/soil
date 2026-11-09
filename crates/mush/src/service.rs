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

pub fn serve(socket: &Path, jobs: &Jobs, idle: Option<Duration>) -> Result<()> {
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
                if let Err(error) = handle(stream, jobs) {
                    eprintln!("mush: request: {error:#}");
                }
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
            match jobs.submit(command, threshold, shell)? {
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
pub fn forward(
    socket: &Path,
    root: &Path,
    desktop: Option<&str>,
    line: &str,
    shell: bool,
    idle: Duration,
    budget: Option<Duration>,
) -> Option<i32> {
    // The caller's patience, not the daemon's: a command the caller would have
    // waited 15 minutes for must not be handed back as a job id after 60s.
    let mut request = json!({
        "kind": if shell { "shell" } else { "exec" },
        "command": line,
    });
    if let Some(budget) = budget {
        request["background_after_secs"] = json!(budget.as_secs());
    }
    let wait = budget.unwrap_or(Duration::from_secs(120)) + Duration::from_secs(5);
    let mut response = request_once(socket, &request, wait);
    if response.is_none() {
        // A socket file with nothing behind it is what a killed daemon leaves.
        let _ = std::fs::remove_file(socket);
        if !start(socket, root, desktop, idle, budget) {
            return None;
        }
        response = request_once(socket, &request, wait);
    }
    let response = response?;
    if response.get("state").and_then(Value::as_str) == Some("background") {
        let hint = response
            .get("hint")
            .and_then(Value::as_str)
            .unwrap_or("command still running");
        eprintln!("{hint}");
        return Some(0);
    }
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Mush service request failed");
        eprintln!("mush: {error}");
        return Some(1);
    }
    if let Some(output) = response.get("output").and_then(Value::as_str) {
        print!("{output}");
    }
    if let Some(stderr) = response.get("stderr").and_then(Value::as_str) {
        eprint!("{stderr}");
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    Some(
        response
            .get("status")
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32,
    )
}

/// One request/response exchange, or `None` when no daemon is listening.
fn request_once(socket: &Path, request: &Value, wait: Duration) -> Option<Value> {
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
