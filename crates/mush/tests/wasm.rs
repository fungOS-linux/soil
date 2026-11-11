use std::{path::PathBuf, process::Command};

use fungos_vfs::Vfs;

/// A root and a content store that belong to this test alone.
///
/// Content is shared across every root on a host by default; a test that does
/// not name its own store writes into the machine's, and then fails only when
/// another test happens to run beside it.
struct Owned {
    dir: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
}

impl Owned {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let store = dir.path().join("store");
        std::fs::create_dir_all(&root).unwrap();
        Self { dir, root, store }
    }

    fn open(&self) -> Vfs {
        Vfs::open_with(
            &self.root,
            fungos_vfs::NodeRole::Full,
            Some(self.store.as_path()),
        )
        .unwrap()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mush"));
        command
            .arg("--root")
            .arg(&self.root)
            .arg("--store")
            .arg(&self.store);
        command
    }
}

/// The spore under test.
///
/// `spores/` is a symlink to the public spores submodule, which a remote build
/// session does not have: bob records a symlink and never follows it. A caller
/// that has built the module elsewhere (a builder, CI) names it here rather than
/// rewriting the tree under the sync.
fn module() -> PathBuf {
    match std::env::var_os("MUSH_TEST_WASM") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spores/mush/module.wasm"),
    }
}

#[test]
fn wasm_spore_loads_mushrc_and_keeps_shell_state() {
    let owned = Owned::new();
    {
        let mut vfs = owned.open();
        vfs.put(
            ".mushrc",
            b"export TERM=mush-test\nexport PS1=\"rc:\\w> \"\nmkdir notes\n",
            "text/plain",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
    }

    let output = owned
        .command()
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg("echo $TERM\nls\ncd notes\npwd\nexit")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mush-test"), "{stdout}");
    assert!(stdout.contains(".mushrc"), "{stdout}");
    assert!(stdout.contains("notes/"), "{stdout}");
    assert!(stdout.contains("/notes"), "{stdout}");
}

#[test]
fn wasm_spore_supports_mkdir_rmdir_and_files() {
    let owned = Owned::new();
    let output = owned
        .command()
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg("mkdir projects\ncd projects\nwrite readme.md hi\ncat readme.md\ncd /\nrmdir projects\npwd")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hi"), "{stdout}");
    assert!(stdout.contains("not-empty"), "{stdout}");
    assert!(stdout.contains("/"), "{stdout}");
}

#[test]
fn wasm_spore_supports_append_grep_wc_tree_and_recursive_rm() {
    let owned = Owned::new();
    let output = owned
        .command()
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg("mkdir docs\nwrite docs/a.md alpha\nappend docs/a.md beta\nhead -n 1 docs/a.md\ntail -n 1 docs/a.md\ngrep beta docs/a.md\nwc -l docs/a.md\ntree\nrm -r docs\ntree")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("alpha"), "{stdout}");
    assert!(stdout.contains("beta"), "{stdout}");
    assert!(stdout.contains("docs/a.md:2:beta"), "{stdout}");
    assert!(stdout.contains("2\t/docs/a.md"), "{stdout}");
    assert!(stdout.contains("docs/"), "{stdout}");
    let vfs = owned.open();
    assert!(
        vfs.file("docs").unwrap().is_none(),
        "recursive rm left docs"
    );
}

#[test]
fn wasm_spore_supports_redirection_recursive_mkdir_cp_and_find() {
    let owned = Owned::new();
    let output = owned
        .command()
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg("mkdir -p a/b/c\necho hello > a/b/c/note.txt\necho world >> a/b/c/note.txt\ncp -r a/b copied\nfind . '*.txt'\ncat copied/c/note.txt")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello"), "{stdout}");
    assert!(stdout.contains("world"), "{stdout}");
    assert!(stdout.contains("note.txt"), "{stdout}");
    let mut vfs = owned.open();
    assert_eq!(vfs.get("copied/c/note.txt").unwrap(), b"hello\nworld\n");
}

#[cfg(unix)]
#[test]
fn run_op_applies_overlay_and_respects_gitignore() {
    use std::os::unix::fs::PermissionsExt;

    let owned = Owned::new();
    {
        let mut vfs = owned.open();
        vfs.put(
            ".gitignore",
            b"target/\n",
            "text/plain",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
        vfs.put(
            "src/main.rs",
            b"old",
            "text/x-rust",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
    }
    let script = owned.dir.path().join("fake-operator.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
           case \"$1\" in --workspace) ws=\"$2\"; shift;; esac\n\
           shift\n\
         done\n\
         printf new > \"$ws/src/main.rs\"\n\
         mkdir -p \"$ws/target/debug\"\n\
         printf binary > \"$ws/target/debug/app\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mush"))
        .arg("run")
        .arg("op")
        .arg("--root")
        .arg(&owned.root)
        .arg("--store")
        .arg(&owned.store)
        .arg("--operator-bin")
        .arg(&script)
        .arg("--prompt")
        .arg("update")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut vfs = owned.open();
    assert_eq!(vfs.get("src/main.rs").unwrap(), b"new");
    assert!(vfs.file("target/debug/app").unwrap().is_none());
}
