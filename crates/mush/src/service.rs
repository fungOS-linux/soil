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
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct Executed {
    pub output: String,
    pub stderr: String,
    pub exit: bool,
    pub status: i32,
}

pub fn serve<F>(socket: &Path, mut execute: F) -> Result<()>
where
    F: FnMut(&str) -> Result<Executed>,
{
    prepare_socket(socket)?;
    let listener = UnixListener::bind(socket)
        .with_context(|| format!("bind Mush service socket {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod Mush service socket {}", socket.display()))?;
    eprintln!("mush: serving on {}", socket.display());
    for connection in listener.incoming() {
        let stream = match connection {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("mush: accept: {error}");
                continue;
            }
        };
        if let Err(error) = handle(stream, &mut execute) {
            eprintln!("mush: request: {error:#}");
        }
    }
    Ok(())
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

fn handle<F>(stream: UnixStream, execute: &mut F) -> Result<()>
where
    F: FnMut(&str) -> Result<Executed>,
{
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
    let Some(command) = request.get("command").and_then(Value::as_str) else {
        let response = json!({"ok": false, "error": "request needs a string `command`"});
        write_response(stream, &response)?;
        return Ok(());
    };
    let mut output = String::new();
    let mut stderr = String::new();
    let mut exit = false;
    let mut status = 0;
    for line in command.lines() {
        let executed = execute(line)?;
        output.push_str(&executed.output);
        stderr.push_str(&executed.stderr);
        exit |= executed.exit;
        status = executed.status;
        if exit {
            break;
        }
    }
    write_response(
        stream,
        &json!({"ok": true, "output": output, "stderr": stderr, "exit": exit, "status": status}),
    )?;
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
