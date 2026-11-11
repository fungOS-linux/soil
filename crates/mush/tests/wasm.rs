use std::{
    path::{Path, PathBuf},
    process::Command,
};

use fungos_vfs::Vfs;

/// Every test runs the binary without a daemon: one started here would outlive
/// the test, keep the shared records store open, and block the next test (or
/// the next `cargo test` run) behind it.
fn mush_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mush"));
    command.env("MUSH_NO_DAEMON", "1");
    command
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

/// One `--command` line, with the exit status the shell reported.
fn run(dir: &Path, command: &str) -> (i32, String, String) {
    let output = mush_command()
        .arg("--root")
        .arg(dir)
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg(command)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
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
            fungos_vfs::Expect::Any,
        )
        .unwrap();
    }

    let output = mush_command()
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
    let output = mush_command()
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("hi"), "{stdout}");
    assert!(stderr.contains("not-empty"), "{stderr}");
    assert!(stdout.contains("/"), "{stdout}");
}

#[test]
fn wasm_spore_supports_append_grep_wc_tree_and_recursive_rm() {
    let dir = tempfile::tempdir().unwrap();
    let output = mush_command()
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
    let output = mush_command()
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

    let output = mush_command()
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
    let output = mush_command()
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
    let output = mush_command()
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
    let output = mush_command()
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
    assert!(
        stdout.contains("name=s.mush first=alpha count=2"),
        "{stdout}"
    );
    assert!(stdout.contains("file\n"), "{stdout}");
    assert!(stdout.contains("math\n"), "{stdout}");
    assert!(stdout.contains("item-a\nitem-b"), "{stdout}");
    assert!(!stdout.contains("never"), "{stdout}");
    assert!(stdout.contains("read=hello"), "{stdout}");
    assert!(stdout.contains("$HOME\n"), "{stdout}");
}

#[test]
fn wasm_spore_supports_loop_control_and_stderr_redirection() {
    let dir = tempfile::tempdir().unwrap();
    let output = mush_command()
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg(
            "for x in a b c; do if test $x = b; then continue; fi; echo item-$x; done\n\
             for x in a b c; do echo hit-$x; break; done\n\
             if true; then echo branch; fi; echo after\n\
             ls /missing 2> /stderr.txt; test -s /stderr.txt; echo redirected\n\
             ls /missing 2>&1; echo merged\n\
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
    assert!(stdout.contains("item-a\nitem-c"), "{stdout}");
    assert!(stdout.contains("hit-a"), "{stdout}");
    assert!(!stdout.contains("hit-b"), "{stdout}");
    assert!(stdout.contains("branch\nafter"), "{stdout}");
    assert!(stdout.contains("redirected"), "{stdout}");
    assert!(stdout.contains("merged"), "{stdout}");
    assert_eq!(stdout.matches("ls: Error").count(), 1, "{stdout}");
}

#[test]
fn wasm_spore_supports_tgrep_over_vfs() {
    let dir = tempfile::tempdir().unwrap();
    let output = mush_command()
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg(
            "mkdir docs\n\
             write docs/a.txt alpha beta\n\
             append docs/a.txt gamma delta\n\
             write docs/b.md ALPHA markdown\n\
             tgrep alpha\n\
             tgrep -i alpha\n\
             tgrep -l alpha\n\
             tgrep -g \"*.md\" alpha\n\
             tgrep --no-index gamma docs\n\
             tgrep definitely-not-there\n\
             echo status=$?\n\
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
    assert!(stdout.contains("/docs/a.txt:1:alpha beta"), "{stdout}");
    assert!(stdout.contains("/docs/b.md:1:ALPHA markdown"), "{stdout}");
    assert!(stdout.contains("/docs/a.txt\n/docs/b.md"), "{stdout}");
    assert!(stdout.contains("/docs/b.md:1:ALPHA markdown"), "{stdout}");
    assert!(stdout.contains("status=1"), "{stdout}");
}

#[test]
fn wasm_spore_keeps_quoted_empty_arguments() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut vfs = Vfs::open(dir.path()).unwrap();
        vfs.put(
            "indented.txt",
            b"  one\n    two\nplain\n",
            "text/plain",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
    }
    // `''` is an argument, not nothing: `echo` shows it as the empty word it is.
    let (status, stdout, _) = run(dir.path(), "echo '' end");
    assert_eq!(status, 0);
    assert_eq!(stdout, " end\n");
    // And `sub` can delete a match, which needs a reachable empty replacement.
    let (status, stdout, stderr) = run(dir.path(), "sub '^  ' '' indented.txt; cat indented.txt");
    assert_eq!(status, 0, "{stdout}{stderr}");
    assert!(stdout.contains("2 replacement(s)"), "{stdout}");
    assert!(stdout.contains("one\n  two\nplain\n"), "{stdout}");
}

#[test]
fn wasm_spore_fetches_through_the_host_allowlist() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();

    let run = |line: &str, allow: &str, token: Option<&str>| {
        let mut command = mush_command();
        command
            .arg("--root")
            .arg(dir.path())
            .arg("--wasm")
            .arg(module())
            .arg("--command")
            .arg(line)
            .env("MUSH_HTTP_ALLOW", allow)
            .env_remove("MUSH_GROUPS")
            .env_remove("MUSH_HTTP_TOKEN_TEST");
        if let Some(token) = token {
            command.env("MUSH_HTTP_TOKEN_TEST", token);
        }
        let output = command.output().unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    // The thread hands back the request head, so a test can see what the host
    // actually sent — which is how the credential assertion works.
    let answer = |body: &'static [u8], status: &'static str| {
        let listener = listener.try_clone().unwrap();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 2048];
            let read = conn.read(&mut buffer).unwrap_or(0);
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            conn.write_all(head.as_bytes()).unwrap();
            conn.write_all(body).unwrap();
            String::from_utf8_lossy(&buffer[..read]).into_owned()
        })
    };

    let served = answer(b"hello", "200 OK");
    let (status, stdout, stderr) = run(&format!("fetch {origin}/hello"), &origin, None);
    let request = served.join().unwrap();
    assert_eq!(status, 0, "{stdout}{stderr}");
    assert_eq!(stdout, "hello");
    assert!(!request.to_lowercase().contains("authorization"), "{request}");

    // A credential handle is resolved by the host; the guest never sees the
    // token and cannot send `authorization` itself.
    let served = answer(b"hello", "200 OK");
    let (status, stdout, stderr) = run(
        &format!("fetch --credential test {origin}/hello"),
        &origin,
        Some("sekret"),
    );
    let request = served.join().unwrap();
    assert_eq!(status, 0, "{stdout}{stderr}");
    assert!(
        request.to_lowercase().contains("authorization: bearer sekret"),
        "{request}"
    );

    // An empty allowlist denies everything: a shell that can fetch anything is
    // ambient authority with extra steps.
    let (status, stdout, stderr) = run(&format!("fetch {origin}/hello"), "", None);
    assert_eq!(status, 1, "{stdout}{stderr}");
    assert!(stderr.contains("denied"), "{stdout}{stderr}");

    // A 4xx is a result, not a success.
    let served = answer(b"missing", "404 Not Found");
    let (status, stdout, stderr) = run(&format!("fetch {origin}/nope"), &origin, None);
    served.join().unwrap();
    assert_eq!(status, 1, "{stdout}{stderr}");
    assert!(stderr.contains("HTTP 404"), "{stdout}{stderr}");
    assert_eq!(stdout, "missing");
}

#[test]
fn wasm_spore_resolves_and_probes_through_the_host() {
    use std::net::TcpListener;

    // A port something is actually listening on, so `ping` proves a connection
    // rather than merely a resolution.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();

    let (status, stdout, _) = run(dir.path(), "dig localhost");
    assert_eq!(status, 0, "{stdout}");
    assert!(
        stdout.contains("127.0.0.1") || stdout.contains("::1"),
        "{stdout}"
    );

    let (status, stdout, _) = run(dir.path(), &format!("ping localhost --port {port}"));
    assert_eq!(status, 0, "{stdout}");
    assert!(stdout.contains(" up ("), "{stdout}");

    // Nothing listens on port 9; that is a "down", not a shell error.
    let (status, stdout, _) = run(dir.path(), "ping localhost --port 9");
    assert_eq!(status, 1, "{stdout}");
    assert!(stdout.contains("no answer"), "{stdout}");

    // `.invalid` is reserved, so this never resolves and never leaves the host.
    let (status, stdout, stderr) = run(dir.path(), "dig does-not-exist.invalid");
    assert_eq!(status, 1);
    assert!(
        stdout.contains("does not resolve") || stderr.contains("does not resolve"),
        "{stdout}{stderr}"
    );
}

#[test]
fn wasm_spore_diff_reports_lines_and_status() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut vfs = Vfs::open(dir.path()).unwrap();
        vfs.put(
            "old.txt",
            b"one\ntwo\nthree\n",
            "text/plain",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
        vfs.put(
            "same.txt",
            b"one\ntwo\nthree\n",
            "text/plain",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
        vfs.put(
            "new.txt",
            b"one\nTWO\nthree\nfour\n",
            "text/plain",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
        // Same lines, no final newline: content a diff must not hide.
        vfs.put(
            "nonl.txt",
            b"one\ntwo\nthree",
            "text/plain",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
    }

    let (status, stdout, _) = run(dir.path(), "diff old.txt new.txt");
    assert_eq!(status, 1, "{stdout}");
    assert!(
        stdout.starts_with("--- /old.txt\n+++ /new.txt\n"),
        "{stdout}"
    );
    assert!(stdout.contains("@@ -1,3 +1,4 @@"), "{stdout}");
    assert!(
        stdout.contains(" one\n-two\n+TWO\n three\n+four\n"),
        "{stdout}"
    );

    let (status, stdout, _) = run(dir.path(), "diff old.txt same.txt");
    assert_eq!(status, 0, "{stdout}");
    assert!(stdout.is_empty(), "{stdout}");

    let (status, stdout, _) = run(dir.path(), "diff -q old.txt new.txt");
    assert_eq!(status, 1);
    assert_eq!(stdout, "Files /old.txt and /new.txt differ\n");

    // Zero context gives one hunk per change, with ranges that match diff(1).
    let (status, stdout, _) = run(dir.path(), "diff -U 0 old.txt new.txt");
    assert_eq!(status, 1);
    assert!(stdout.contains("@@ -2 +2 @@"), "{stdout}");
    assert!(stdout.contains("@@ -3,0 +4 @@"), "{stdout}");

    let (status, stdout, _) = run(dir.path(), "diff old.txt nonl.txt");
    assert_eq!(status, 1);
    assert!(
        stdout.contains("\\ No newline at end of file"),
        "{stdout}"
    );

    // Unreadable is status 2, which keeps it distinguishable from "differ".
    let (status, stdout, stderr) = run(dir.path(), "diff old.txt missing.txt");
    assert_eq!(status, 2, "{stdout}{stderr}");
    assert!(stderr.contains("missing.txt"), "{stderr}");
}

#[test]
fn wasm_spore_json_reads_and_edits_documents() {
    let dir = tempfile::tempdir().unwrap();
    let config = concat!(
        "{\n",
        "  \"name\": \"mush\",\n",
        "  \"nested\": {\n",
        "    \"count\": 1\n",
        "  },\n",
        "  \"list\": [\n",
        "    1,\n",
        "    2\n",
        "  ]\n",
        "}\n",
    );
    {
        let mut vfs = Vfs::open(dir.path()).unwrap();
        vfs.put(
            "config.json",
            config.as_bytes(),
            "application/json",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
        vfs.put(
            "patch.json",
            b"{\"nested\":{\"count\":2,\"extra\":true}}\n",
            "application/json",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
        // Compact and without a final newline: an edit must keep both.
        vfs.put(
            "tiny.json",
            b"{\"a\":1}",
            "application/json",
            fungos_vfs::Expect::Any,
        )
        .unwrap();
    }

    // A string prints raw so it can be used as a shell word; -j keeps the JSON.
    let (status, stdout, _) = run(
        dir.path(),
        "json get config.json name\n\
         json get -j config.json name\n\
         json get config.json nested.count\n\
         json get config.json list[1]\n\
         json get config.json nested",
    );
    assert_eq!(status, 0, "{stdout}");
    assert_eq!(stdout, "mush\n\"mush\"\n1\n2\n{\"count\":1}\n", "{stdout}");

    let (status, _, stderr) = run(dir.path(), "json get config.json nope");
    assert_eq!(status, 1);
    assert!(stderr.contains("no value at \"nope\""), "{stderr}");

    // set keeps unrelated fields, key order, and the file's own indentation.
    let (status, stdout, _) = run(dir.path(), "json set config.json nested.count 7");
    assert_eq!(status, 0, "{stdout}");
    let mut vfs = Vfs::open(dir.path()).unwrap();
    assert_eq!(
        String::from_utf8(vfs.get("config.json").unwrap()).unwrap(),
        config.replace("\"count\": 1", "\"count\": 7")
    );

    // A new key is appended at the end; a compact file stays compact.
    run(dir.path(), "json set tiny.json b true");
    let mut vfs = Vfs::open(dir.path()).unwrap();
    assert_eq!(vfs.get("tiny.json").unwrap(), b"{\"a\":1,\"b\":true}");

    // del removes a key or an array element (shifting the rest down).
    run(dir.path(), "json del config.json list[0]");
    let mut vfs = Vfs::open(dir.path()).unwrap();
    let text = String::from_utf8(vfs.get("config.json").unwrap()).unwrap();
    assert!(text.contains("\"list\": [\n    2\n  ]"), "{text}");

    // merge is deep: nested keys merge, scalars are replaced.
    let (status, _, stderr) = run(dir.path(), "json merge config.json patch.json");
    assert_eq!(status, 0, "{stderr}");
    let mut vfs = Vfs::open(dir.path()).unwrap();
    let text = String::from_utf8(vfs.get("config.json").unwrap()).unwrap();
    assert!(text.contains("\"count\": 2"), "{text}");
    assert!(text.contains("\"extra\": true"), "{text}");
    assert!(text.contains("\"name\": \"mush\""), "{text}");

    // pretty prints; it never writes, and it reads stdin without a file.
    assert_eq!(
        run(dir.path(), "json pretty --indent 0 tiny.json").1,
        "{\"a\":1,\"b\":true}\n"
    );
    assert_eq!(
        run(dir.path(), "echo '{\"k\":[1,2]}' | json pretty").1,
        "{\n  \"k\": [\n    1,\n    2\n  ]\n}\n"
    );

    // `-` takes the new value from stdin, so a pipeline can feed `set` too.
    let (status, _, stderr) = run(
        dir.path(),
        "echo '{\"from\":\"stdin\"}' | json set config.json added -",
    );
    assert_eq!(status, 0, "{stderr}");
    let mut vfs = Vfs::open(dir.path()).unwrap();
    let text = String::from_utf8(vfs.get("config.json").unwrap()).unwrap();
    assert!(
        text.contains("\"added\": {\n    \"from\": \"stdin\"\n  }"),
        "{text}"
    );

    // Invalid documents and unquoted strings fail loudly instead of guessing.
    let (status, _, stderr) = run(dir.path(), "json set config.json name bob");
    assert_eq!(status, 1);
    assert!(stderr.contains("VALUE must be JSON"), "{stderr}");
    let (status, _, stderr) = run(dir.path(), "json get patch.json nested.count.deep");
    assert_eq!(status, 1);
    assert!(stderr.contains("no value at"), "{stderr}");
}

#[test]
fn wasm_spore_supports_desktop_registry_and_project_mount() {
    let dir = tempfile::tempdir().unwrap();
    let output = mush_command()
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .arg("--command")
        .arg("mkdir docs\nwrite docs/a.txt hello\ncat docs/a.txt\nexit")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status {:?} stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello"), "{stdout}");
}

#[test]
fn command_that_outlives_its_deadline_is_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let output = mush_command()
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .arg("--timeout")
        .arg("0.2")
        .arg("--command")
        .arg("while true; do write f.txt hello; done")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(124),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("exceeded"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn zero_timeout_disables_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let output = mush_command()
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .arg("--timeout")
        .arg("0")
        .arg("--command")
        .arg("echo still-runs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("still-runs"));
}

#[test]
fn server_subcommand_help_does_not_start_a_server() {
    let output = mush_command()
        .arg("mcp")
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"), "{stdout}");
    assert!(stdout.contains("--foreground"), "{stdout}");
}

#[test]
fn server_refuses_to_start_without_a_terminal_or_foreground() {
    let dir = tempfile::tempdir().unwrap();
    // Command::output() gives the child a pipe, so stdin is not a terminal.
    let output = mush_command()
        .arg("mcp")
        .arg("--root")
        .arg(dir.path())
        .arg("--wasm")
        .arg(module())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is a server"), "{stderr}");
    assert!(stderr.contains("--foreground"), "{stderr}");
}

#[test]
fn exit_status_reports_what_the_command_did() {
    let dir = tempfile::tempdir().unwrap();
    let status = |command: &str| {
        mush_command()
            .arg("--root")
            .arg(dir.path())
            .arg("--wasm")
            .arg(module())
            .arg("--command")
            .arg(command)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .code()
            .unwrap()
    };
    assert_eq!(status("echo hi"), 0);
    assert_eq!(status("true"), 0);
    // A harness that only reads the exit code must not see success.
    assert_eq!(status("false"), 1);
    assert_eq!(status("no-such-command"), 1);
    assert_eq!(status("cat missing.txt"), 1);
    assert_eq!(status("true; false"), 1);
    assert_eq!(status("false; true"), 0);
}

#[test]
fn origin_resolves_a_desktop_but_refuses_to_call_it_ready() {
    let home = tempfile::tempdir().unwrap();
    let registry = home.path().join(".config/fungos/desktops");
    std::fs::create_dir_all(&registry).unwrap();
    let mount = home.path().join("mnt/personal");
    std::fs::write(
        registry.join("personal.toml"),
        format!(
            "id = \"personal\"\nname = \"Personal\"\nmount = \"{}\"\nprojects = \"soil\"\n",
            mount.display()
        ),
    )
    .unwrap();
    // The projection path exists, but nothing is mounted there.
    std::fs::create_dir_all(mount.join("projects/soil")).unwrap();

    let checkout = home.path().join("soil/.delta/worktrees/abc123/soil");
    std::fs::create_dir_all(&checkout).unwrap();

    let output = mush_command()
        .arg("origin")
        .arg("--json")
        .arg("--cwd")
        .arg(&checkout)
        .env("HOME", home.path())
        .env("DELTA_CURRENT_THREAD_ID", "thread-under-test")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a path that is not a mount must not report ready: {stdout}"
    );
    assert!(stdout.contains("\"desktop\":\"personal\""), "{stdout}");
    assert!(stdout.contains("\"delta_worktree\":\"abc123\""), "{stdout}");
    assert!(
        stdout.contains("\"thread\":\"thread-under-test\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"readiness\":\"not-mounted\""), "{stdout}");
}

#[test]
fn origin_reports_an_unregistered_path() {
    let home = tempfile::tempdir().unwrap();
    let elsewhere = home.path().join("somewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let output = mush_command()
        .arg("origin")
        .arg("--json")
        .arg("--cwd")
        .arg(&elsewhere)
        .env("HOME", home.path())
        .env_remove("DELTA_CURRENT_THREAD_ID")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"readiness\":\"unregistered\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"desktop\":null"), "{stdout}");
}
