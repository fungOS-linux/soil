use std::{path::PathBuf, process::Command};

use fungos_vfs::Vfs;

fn module() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spores/mush/module.wasm")
}

#[test]
fn wasm_spore_loads_mushrc_and_keeps_shell_state() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut vfs = Vfs::open(dir.path()).unwrap();
        vfs.put(
            ".mushrc",
            b"export TERM=mush-test\nexport PS1=\"rc:\\w> \"\nmkdir notes\n",
            "text/plain",
            None,
        )
        .unwrap();
    }

    let output = Command::new(env!("CARGO_BIN_EXE_mush"))
        .arg("--root")
        .arg(dir.path())
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
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mush"))
        .arg("--root")
        .arg(dir.path())
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
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mush"))
        .arg("--root")
        .arg(dir.path())
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
    let vfs = Vfs::open(dir.path()).unwrap();
    assert!(
        vfs.file("docs").unwrap().is_none(),
        "recursive rm left docs"
    );
}

#[test]
fn wasm_spore_supports_redirection_recursive_mkdir_cp_and_find() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mush"))
        .arg("--root")
        .arg(dir.path())
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
    let mut vfs = Vfs::open(dir.path()).unwrap();
    assert_eq!(vfs.get("copied/c/note.txt").unwrap(), b"hello\nworld\n");
}

#[cfg(unix)]
#[test]
fn run_op_applies_overlay_and_respects_gitignore() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    {
        let mut vfs = Vfs::open(dir.path()).unwrap();
        vfs.put(".gitignore", b"target/\n", "text/plain", None)
            .unwrap();
        vfs.put("src/main.rs", b"old", "text/x-rust", None).unwrap();
    }
    let script = dir.path().join("fake-operator.sh");
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
        .arg(dir.path())
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
    let mut vfs = Vfs::open(dir.path()).unwrap();
    assert_eq!(vfs.get("src/main.rs").unwrap(), b"new");
    assert!(vfs.file("target/debug/app").unwrap().is_none());
}

#[test]
fn wasm_spore_supports_pipelines_sequences_and_input_redirection() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mush"))
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg(
            "mkdir docs\n\
             write docs/a.txt alpha\n\
             write docs/b.txt beta\n\
             cat docs/a.txt | grep alpha\n\
             echo hello | wc -l\n\
             false || echo recovered\n\
             true && echo ran\n\
             false && echo skipped\n\
             echo one; echo two\n\
             cat < docs/a.txt\n\
             echo x > docs/out.txt\n\
             echo y >> docs/out.txt\n\
             cat docs/out.txt\n\
             false\n\
             echo $?\n\
             exit",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("alpha"), "{stdout}");
    assert!(stdout.contains("1\n"), "{stdout}");
    assert!(stdout.contains("recovered"), "{stdout}");
    assert!(stdout.contains("ran"), "{stdout}");
    assert!(!stdout.contains("skipped"), "{stdout}");
    assert!(stdout.contains("one\ntwo"), "{stdout}");
    assert!(stdout.contains("x\ny\n"), "{stdout}");
    assert!(stdout.trim_end().ends_with('1'), "{stdout}");
}

#[test]
fn wasm_spore_supports_globs_substitution_comments_and_printf() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mush"))
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg(
            "mkdir docs\n\
             write docs/a.txt alpha\n\
             write docs/b.md beta\n\
             write docs/c.txt gamma\n\
             echo docs/*.txt\n\
             echo docs/[ab].*\n\
             echo \"docs/*.txt\"\n\
             echo $(echo hi)\n\
             echo pre$(cat docs/a.txt)post\n\
             printf '%s:%d\\n' age 42\n\
             echo after # ignored comment\n\
             exit",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("docs/a.txt docs/c.txt"), "{stdout}");
    assert!(stdout.contains("docs/a.txt docs/b.md"), "{stdout}");
    assert!(stdout.contains("docs/*.txt\n"), "{stdout}");
    assert!(stdout.contains("hi\n"), "{stdout}");
    assert!(stdout.contains("prealphapost\n"), "{stdout}");
    assert!(stdout.contains("age:42\n"), "{stdout}");
    assert!(stdout.contains("after\n"), "{stdout}");
    assert!(!stdout.contains("ignored"), "{stdout}");
}

#[test]
fn wasm_spore_supports_control_flow_test_read_and_script_args() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mush"))
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg(
            "mkdir docs\n\
             write docs/a.txt hello\n\
             write s.mush 'echo name=$0 first=$1 count=$#'\n\
             source s.mush alpha beta\n\
             if test -f docs/a.txt; then echo file; else echo missing; fi\n\
             if [ 3 -gt 2 ]; then echo math; fi\n\
             for x in a b; do echo item-$x; done\n\
             while false; do echo never; done\n\
             printf 'hello\\nworld\\n' | read x; echo read=$x\n\
             echo '$HOME'\n\
             exit",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("name=s.mush first=alpha count=2"), "{stdout}");
    assert!(stdout.contains("file\n"), "{stdout}");
    assert!(stdout.contains("math\n"), "{stdout}");
    assert!(stdout.contains("item-a\nitem-b"), "{stdout}");
    assert!(!stdout.contains("never"), "{stdout}");
    assert!(stdout.contains("read=hello"), "{stdout}");
    assert!(stdout.contains("$HOME\n"), "{stdout}");
}
