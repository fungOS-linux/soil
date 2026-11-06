//! Unix-socket framing shared by the broker daemon and clients.

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{Decision, Request};

pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Decision { decision: Decision },
    Error { error: String },
}

pub fn write_response(mut stream: UnixStream, response: &Response) -> std::io::Result<()> {
    let encoded = serde_json::to_vec(response).map_err(std::io::Error::other)?;
    if encoded.len() > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "response too large",
        ));
    }
    stream.write_all(&encoded)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

pub fn read_request(stream: &UnixStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    reader
        .by_ref()
        .take(MAX_FRAME as u64 + 1)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.len() > MAX_FRAME {
        return Err("request too large".into());
    }
    if !line.ends_with('\n') {
        return Err("request is not newline terminated".into());
    }
    serde_json::from_str(&line).map_err(|e| format!("invalid request: {e}"))
}

pub fn read_response(stream: &UnixStream) -> Result<Response, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    reader
        .by_ref()
        .take(MAX_FRAME as u64 + 1)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.len() > MAX_FRAME {
        return Err("response too large".into());
    }
    if !line.ends_with('\n') {
        return Err("response is not newline terminated".into());
    }
    serde_json::from_str(&line).map_err(|e| format!("invalid response: {e}"))
}

/// The socket must be a real socket, owned by this user, with no group/other
/// permission bits. The parent must not be group/other writable.
pub fn validate_socket(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_socket() {
        return Err("consent path is not a socket".into());
    }
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err("consent socket is not owned by this user".into());
    }
    if meta.mode() & 0o077 != 0 {
        return Err("consent socket permissions are too broad".into());
    }
    let parent = path.parent().ok_or("consent socket has no parent")?;
    let parent_meta = std::fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
    if !parent_meta.is_dir() {
        return Err("consent socket parent is not a directory".into());
    }
    if parent_meta.uid() != unsafe { libc::geteuid() } {
        return Err("consent socket parent is not owned by this user".into());
    }
    if parent_meta.mode() & 0o022 != 0 {
        return Err("consent socket parent is group/other writable".into());
    }
    Ok(())
}

/// Create a private directory for the socket. Existing directories are
/// validated rather than silently trusted.
pub fn private_socket_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("fungos-consent-{}", unsafe { libc::geteuid() }));
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(format!("{} is not a directory", dir.display()));
            }
            if meta.uid() != unsafe { libc::geteuid() } {
                return Err(format!("{} is not owned by this user", dir.display()));
            }
            if meta.mode() & 0o077 != 0 {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| e.to_string())?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(&dir).map_err(|e| e.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(dir)
}

pub fn default_socket_path() -> Result<PathBuf, String> {
    Ok(private_socket_dir()?.join("consent.sock"))
}

/// Verify that the peer on the other end of the socket has the same effective
/// user id. The socket permissions already restrict access; this is defense in
/// depth and catches an inherited/misconfigured descriptor.
pub fn peer_is_self(stream: &UnixStream) -> Result<bool, String> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(cred.uid == unsafe { libc::geteuid() })
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd"))]
    {
        let mut uid = 0;
        let mut gid = 0;
        let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(uid == unsafe { libc::geteuid() })
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd"
    )))]
    {
        let _ = fd;
        Ok(true)
    }
}

pub fn prepare_socket_path(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if let Some(name) = parent.file_name().and_then(|name| name.to_str())
            && name.starts_with("fungos-consent-")
        {
            return Ok(());
        }
        return Err("consent socket must live in the private fungos-consent directory".into());
    }
    Err("consent socket has no parent".into())
}
