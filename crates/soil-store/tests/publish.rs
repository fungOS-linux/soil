//! End-to-end publishing: the operator delegates a key, the publisher signs the
//! entries, the client verifies, and a revocation withdraws the key.
//!
//! These run the real binary against a real GnuPG home, because the thing worth
//! proving is that the bytes the signer writes are the bytes the verifier reads.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use soil_store::Index;

const PUBLISHER: &str = "mycelium:test/publisher";
const DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const CID: &str = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";

fn gpg_available() -> bool {
    Command::new("gpg")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn gpg(home: &Path, args: &[&str]) -> Output {
    let output = Command::new("gpg")
        .arg("--homedir")
        .arg(home)
        .args(["--batch", "--no-tty", "--yes"])
        .args(args)
        .output()
        .expect("run gpg");
    assert!(
        output.status.success(),
        "gpg {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn keygen(home: &Path, uid: &str) -> String {
    gpg(
        home,
        &[
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            uid,
            "ed25519",
            "sign",
            "0",
        ],
    );
    let listing = gpg(home, &["--with-colons", "--list-secret-keys"]);
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("fpr:"))
        .and_then(|rest| rest.split(':').nth(8))
        .map(str::to_owned)
        .expect("fingerprint")
}

fn export(home: &Path, fingerprint: &str, path: &Path) {
    let output = gpg(home, &["--armor", "--export", fingerprint]);
    std::fs::write(path, output.stdout).expect("write public key");
}

fn store(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_soil-store"))
        .args(args)
        .output()
        .expect("run soil-store")
}

fn store_ok(args: &[&str]) -> Output {
    let output = store(args);
    assert!(
        output.status.success(),
        "soil-store {args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn index_file(dir: &Path) -> PathBuf {
    let path = dir.join("index.json");
    let index = serde_json::json!({
        "schema": 1,
        "source": "test",
        "checkpoint": { "seq": 1, "root": "unsigned", "signature": "" },
        "entries": [{
            "seq": 1,
            "kind": "add",
            "spore_id": "dev.fungos.demo",
            "version": "0.1.0",
            "digest": DIGEST,
            "package": CID,
            "grants": ["soil:canvas"],
            "publisher": null,
            "publisher_key": null,
            "signature": ""
        }]
    });
    std::fs::write(&path, index.to_string()).expect("write index");
    path
}

#[test]
fn delegate_sign_verify_and_revoke() {
    if !gpg_available() {
        eprintln!("gpg unavailable; skipping publishing test");
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("gnupg");
    std::fs::create_dir(&home).expect("gnupg home");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
            .expect("protect gnupg home");
    }
    let operator = keygen(&home, "operator <operator@example.invalid>");
    let publisher = keygen(&home, "publisher <publisher@example.invalid>");
    let operator_public = temp.path().join("operator.asc");
    let publisher_public = temp.path().join("publisher.asc");
    export(&home, &operator, &operator_public);
    export(&home, &publisher, &publisher_public);

    let index = index_file(temp.path());
    let index_arg = index.to_str().unwrap();
    let home_arg = home.to_str().unwrap();
    let operator_public_arg = operator_public.to_str().unwrap();

    // An unsigned index is structurally fine and unverifiable: nothing delegates
    // the publisher yet, and no entry carries a signature.
    assert!(
        !store(&[
            "verify",
            "--index",
            index_arg,
            "--operator-key",
            operator_public_arg
        ])
        .status
        .success()
    );

    let delegation = temp.path().join("delegation.json");
    store_ok(&[
        "delegate",
        "--publisher",
        PUBLISHER,
        "--operator-key",
        &operator,
        "--public-key",
        publisher_public.to_str().unwrap(),
        "--gnupg-home",
        home_arg,
        "--days",
        "30",
        "-o",
        delegation.to_str().unwrap(),
    ]);
    let delegation_text = std::fs::read_to_string(&delegation).expect("read delegation");
    let parsed: soil_store::Delegation = serde_json::from_str(&delegation_text).expect("parse");
    assert_eq!(parsed.fingerprint, publisher);
    assert_eq!(parsed.publisher, PUBLISHER);
    assert!(parsed.expires_at > parsed.issued_at);

    store_ok(&[
        "sign",
        "--index",
        index_arg,
        "--delegation",
        delegation.to_str().unwrap(),
        "--publisher",
        PUBLISHER,
        "--key",
        &publisher,
        "--countersign",
        &operator,
        "--gnupg-home",
        home_arg,
        "--write",
    ]);
    store_ok(&[
        "verify",
        "--index",
        index_arg,
        "--operator-key",
        operator_public_arg,
    ]);

    // Changing an acted-on field after signing is visible to the verifier.
    let signed = std::fs::read_to_string(&index).expect("read index");
    let tampered = signed.replace("\"0.1.0\"", "\"0.9.9\"");
    assert_ne!(signed, tampered);
    std::fs::write(&index, &tampered).expect("write tampered index");
    assert!(
        !store(&[
            "verify",
            "--index",
            index_arg,
            "--operator-key",
            operator_public_arg
        ])
        .status
        .success()
    );
    std::fs::write(&index, &signed).expect("restore index");

    store_ok(&[
        "revoke",
        "--index",
        index_arg,
        "--publisher",
        PUBLISHER,
        "--key",
        &publisher,
        "--reason",
        "laptop lost",
        "--operator-key",
        &operator,
        "--gnupg-home",
        home_arg,
        "--write",
    ]);
    store_ok(&[
        "verify",
        "--index",
        index_arg,
        "--operator-key",
        operator_public_arg,
    ]);
    let revoked = Index::read(&index).expect("read revoked index");
    assert!(
        revoked.catalog().is_empty(),
        "revoked packages leave the store"
    );
    assert_eq!(revoked.revocations.len(), 1);
}
