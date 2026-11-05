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
