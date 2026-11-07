//! Shared helpers for the soil-runtime integration tests.
//!
//! These files each bind a Unix socket to stand in for a host provider. A Unix
//! socket path is capped at 104 bytes on macOS, and `TMPDIR` can be longer than
//! that on its own, so binding under the ambient temp dir makes a test fail for
//! a reason that has nothing to do with what it is testing. Every socket in this
//! directory goes through [`socket_path`] instead.

// Each test binary compiles this module separately and uses a different subset
// of it, so a helper that one binary does not call is not dead code.
#![allow(dead_code)]

use std::path::PathBuf;

/// A short, process-unique working directory for a test binary.
///
/// The directory is created once and left behind: tests in one binary run in
/// parallel, so a cleanup that races another test would be worse than a few
/// bytes under `/tmp`.
pub fn work_dir(name: &str) -> PathBuf {
    let directory = std::path::Path::new("/tmp")
        .join(format!("soil-c-{}", std::process::id()))
        .join(name);
    std::fs::create_dir_all(&directory).expect("create test directory");
    directory
}

/// A short, process-unique path for a test socket.
pub fn socket_path(name: &str) -> PathBuf {
    work_dir("sockets").join(name)
}
