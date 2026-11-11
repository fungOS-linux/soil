//! `mush build` is the seam between a Spore and the host's transport.
//!
//! A Spore has no sockets, so the only thing it can do about a remote build is
//! ask. These tests use a stub `fpl-bob`: what matters here is the contract —
//! the argv the host hands over, the session identity, and what comes back
//! through the overlay.

use std::{path::PathBuf, process::Command};

fn mush() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mush"))
}

/// A stub bob that records its argv and, when told to, writes a file into the
/// workspace — which is how a real build's source mutations arrive.
fn stub_bob(dir: &std::path::Path, write: Option<&str>) -> PathBuf {
    let path = dir.join("fpl-bob");
    let body = match write {
        Some(name) => format!("#!/bin/sh\nprintf '%s\\n' \"$@\"\nprintf generated > {name}\n"),
        None => "#!/bin/sh\nprintf '%s\\n' \"$@\"\n".to_string(),
    };
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn run(
    dir: &std::path::Path,
    store: &std::path::Path,
    stub_dir: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    let path = format!(
        "{}:{}",
        stub_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // The same store the test reads back from: content is shared by default,
    // and a test that does not name one writes into the machine's.
    Command::new(mush())
        .arg("--root")
        .arg(dir)
        .arg("--store")
        .arg(store)
        .arg("build")
        .args(args)
        .env("PATH", path)
        .output()
        .unwrap()
}

#[test]
fn build_hands_bob_a_worktree_session_and_no_copy_back() {
    let dir = tempfile::tempdir().unwrap();
    let stub = stub_bob(dir.path(), None);
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let output = run(
        &root,
        &dir.path().join("store"),
        stub.parent().unwrap(),
        &["--", "cargo", "test"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let argv = String::from_utf8_lossy(&output.stdout);
    let argv = argv.lines().collect::<Vec<_>>();
    assert_eq!(argv[0], "remote");
    let session = argv
        .iter()
        .position(|arg| *arg == "--session")
        .map(|index| argv[index + 1])
        .unwrap_or_default();
    // The session names the worktree, not the overlay: a warm remote target has
    // to survive across runs.
    assert!(session.starts_with("mush-root-"), "{argv:?}");
    // Build output never travels back.
    assert!(argv.windows(2).any(|w| w == ["--back", ""]), "{argv:?}");
    let sep = argv.iter().position(|a| *a == "--").unwrap();
    assert_eq!(&argv[sep + 1..], &["cargo", "test"]);
}

#[test]
fn build_applies_what_the_build_wrote_back_to_the_vfs() {
    let dir = tempfile::tempdir().unwrap();
    let stub = stub_bob(dir.path(), Some("generated.txt"));
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let output = run(
        &root,
        &dir.path().join("store"),
        stub.parent().unwrap(),
        &["--", "cargo", "test"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The build ran in an overlay; the file it wrote is in the VFS now.
    let mut vfs = fungos_vfs::Vfs::open_with(
        &root,
        fungos_vfs::NodeRole::Full,
        Some(&dir.path().join("store")),
    )
    .unwrap();
    assert_eq!(vfs.get("generated.txt").unwrap(), b"generated");
}

#[test]
fn build_without_a_command_explains_the_usage() {
    let dir = tempfile::tempdir().unwrap();
    let stub = stub_bob(dir.path(), None);
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let output = run(
        &root,
        &dir.path().join("store"),
        stub.parent().unwrap(),
        &[],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mush build"), "{stderr}");
}
