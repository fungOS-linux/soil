use soil_spore::{
    Error, Manifest, check, compute_lock, pack, publish_blockers, verify, write_lock,
};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const MANIFEST: &str = r#"schema = 1

[spore]
id = "dev.fungos.markdown"
name = "markdown"
version = "0.1.0"
license = "MIT"
summary = "demo spore"

[engine]
soil = ">=0.1"
target = "wasm32-wasip2"

[module]
path = "module.wasm"

[ui]
entry = "ui/editor.lua"

[launch]
default = "canvas"
surfaces = ["web", "native", "canvas"]

[grants]
"soil:canvas" = "present the surface"

[assets]
"site-map" = { path = "assets/site-map.png", media_type = "image/png" }
"#;

fn fixture() -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("soil-spore-test-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("ui")).unwrap();
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::write(root.join("spore.toml"), MANIFEST).unwrap();
    fs::write(root.join("module.wasm"), b"\0asm\x01\0\0\0").unwrap();
    fs::write(root.join("ui/editor.lua"), b"return {}\n").unwrap();
    fs::write(root.join("assets/site-map.png"), b"\x89PNG\r\n").unwrap();
    root
}

fn build_tar(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_username("").unwrap();
        header.set_groupname("").unwrap();
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(bytes))
            .unwrap();
    }
    builder.into_inner().unwrap()
}

#[test]
fn check_passes_and_lock_is_deterministic() {
    let root = fixture();
    let first = check(&root).unwrap();
    let second = write_lock(&root).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.files.len(), 4);
    assert_eq!(
        first
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "assets/site-map.png",
            "module.wasm",
            "spore.toml",
            "ui/editor.lua"
        ]
    );
    assert_eq!(first.digest.len(), 64);
}

#[test]
fn a_stale_lock_is_refused_until_refreshed() {
    let root = fixture();
    write_lock(&root).unwrap();
    fs::write(root.join("ui/editor.lua"), b"return { restyled = true }\n").unwrap();
    assert!(matches!(check(&root), Err(Error::Lock(_))));
    write_lock(&root).unwrap();
    check(&root).unwrap();
}

#[test]
fn packing_is_byte_identical() {
    let root = fixture();
    let first = pack(&root).unwrap();
    let second = pack(&root).unwrap();
    assert_eq!(first, second, "repacking must not change a byte");
}

#[test]
fn verify_roundtrips_a_package() {
    let root = fixture();
    let bytes = pack(&root).unwrap();
    let (manifest, lock) = verify(&bytes).unwrap();
    assert_eq!(manifest.spore.id, "dev.fungos.markdown");
    assert_eq!(lock.version, "0.1.0");
    assert!(lock.file("module.wasm").is_some());
    assert!(
        lock.file("spore.lock").is_none(),
        "the lock does not cover itself"
    );
}

#[test]
fn verify_rejects_a_modified_member() {
    let root = fixture();
    let manifest = Manifest::load(&root).unwrap();
    let lock = compute_lock(&root, &manifest).unwrap();

    let mut files = soil_spore::collect(&root).unwrap();
    for (path, bytes) in &mut files {
        if path == "module.wasm" {
            bytes.extend_from_slice(b"tampered");
        }
    }
    files.push(("spore.lock".to_owned(), lock.render().into_bytes()));
    let bytes = build_tar(&files);

    assert!(matches!(verify(&bytes), Err(Error::Lock(_))));
}

#[test]
fn manifest_refuses_traversal_and_bad_identity() {
    let bad_paths = MANIFEST.replace("path = \"module.wasm\"", "path = \"../module.wasm\"");
    assert!(Manifest::parse(&bad_paths).is_err());

    let absolute = MANIFEST.replace("path = \"module.wasm\"", "path = \"/module.wasm\"");
    assert!(Manifest::parse(&absolute).is_err());

    let bad_id = MANIFEST.replace("dev.fungos.markdown", "Markdown");
    assert!(Manifest::parse(&bad_id).is_err());

    let bad_surface = MANIFEST.replace(
        "surfaces = [\"web\", \"native\", \"canvas\"]",
        "surfaces = [\"vga\"]",
    );
    assert!(Manifest::parse(&bad_surface).is_err());

    let bad_grant = MANIFEST.replace("\"soil:canvas\"", "\"root\"");
    assert!(Manifest::parse(&bad_grant).is_err());

    let missing_asset = MANIFEST.replace("assets/site-map.png", "assets/absent.png");
    assert!(
        Manifest::parse(&missing_asset).is_ok(),
        "parse is not a filesystem check"
    );
}

#[test]
fn a_symlinked_file_is_refused() {
    let root = fixture();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("module.wasm"), root.join("link.wasm")).unwrap();
        assert!(matches!(
            soil_spore::collect(&root),
            Err(Error::UnsafePath(_))
        ));
    }
}

#[test]
fn tar_non_regular_entries_are_refused() {
    // A tar can carry links and devices; a package may only carry files.
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_link(&mut header, "module.wasm", "/etc/passwd")
        .unwrap();
    let bytes = builder.into_inner().unwrap();

    assert!(matches!(verify(&bytes), Err(Error::UnsafePath(_))));
}

#[test]
fn a_package_without_a_lock_is_refused() {
    let root = fixture();
    let files = soil_spore::collect(&root).unwrap();
    let bytes = build_tar(&files);
    assert!(matches!(verify(&bytes), Err(Error::Lock(_))));
}

#[test]
fn the_manifest_and_lock_must_agree() {
    let root = fixture();
    let manifest = Manifest::load(&root).unwrap();
    let lock = soil_spore::Lock::new("dev.fungos.other", "0.1.0", Vec::new());
    assert_ne!(manifest.spore.id, lock.id);
    let _ = compute_lock(&root, &manifest).unwrap();
}

#[test]
fn safe_paths_are_the_ones_we_accept() {
    for good in ["module.wasm", "ui/editor.lua", "a/b/c.png", "./x"] {
        assert!(soil_spore::safe_relative(good).is_ok(), "{good}");
    }
    for bad in ["", "/etc/passwd", "../x", "a/../../b", "a\\b", "a\0b"] {
        assert!(soil_spore::safe_relative(bad).is_err(), "{bad}");
    }
    let _ = Path::new("x");
}

const CID: &str = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";

#[test]
fn a_package_ships_only_what_it_declares() {
    let root = fixture();
    // Build inputs and repo documentation are not payload.
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("ui/lib")).unwrap();
    fs::write(root.join("src/lib.rs"), b"pub fn main() {}\n").unwrap();
    fs::write(root.join("README.md"), b"# demo\n").unwrap();
    fs::write(root.join("screenshot.png"), b"\x89PNG\r\n").unwrap();
    fs::write(root.join("ui/lib/ui.lua"), b"return {}\n").unwrap();

    let lock = write_lock(&root).unwrap();
    assert_eq!(
        lock.files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "assets/site-map.png",
            "module.wasm",
            "spore.toml",
            "ui/editor.lua",
            "ui/lib/ui.lua",
        ],
        "a package carries runtime files and every .lua the UI is made of"
    );
}

#[test]
fn store_artwork_is_a_cid_and_never_a_path() {
    let with = |extra: &str| MANIFEST.replace("[engine]", &format!("{extra}\n[engine]"));
    let icon = |value: &str| {
        with(&format!(
            "[presentation]\nbadge = \"D\"\ncolor = \"#8bd5ca\"\nicon = \"{value}\""
        ))
    };

    assert!(
        Manifest::parse(&icon(CID)).is_ok(),
        "a content-addressed mark is a reference"
    );
    assert!(
        Manifest::parse(&icon("assets/icon.png")).is_err(),
        "a path here puts the bytes back in the bundle"
    );
    assert!(
        Manifest::parse(&icon("ipfs://not-a-cid")).is_err(),
        "ipfs:// is not enough; the CID must parse"
    );

    let screenshot = |value: &str| {
        with(&format!(
            "[catalog]\ncategories = [\"utilities\"]\nscreenshots = [ {{ cid = \"{value}\", caption = \"A screen\" }} ]"
        ))
    };
    assert!(Manifest::parse(&screenshot(CID)).is_ok());
    assert!(Manifest::parse(&screenshot("screens/one.png")).is_err());
}

#[test]
fn a_listing_needs_a_shape_before_it_is_reviewed() {
    let plain = Manifest::parse(MANIFEST).unwrap();
    let blockers = publish_blockers(&plain);
    assert!(
        blockers
            .iter()
            .any(|blocker| blocker.contains("description")),
        "a listing without a paragraph is not a listing: {blockers:?}"
    );
    assert!(
        blockers.iter().any(|blocker| blocker.contains("catalog")),
        "a package with no listing is not publishable: {blockers:?}"
    );

    let listing = MANIFEST
        .replace(
            "summary = \"demo spore\"",
            "summary = \"demo spore\"\ndescription = \"A demo Spore for the tests.\"\nhomepage = \"https://example.test\"",
        )
        .replace(
            "[engine]",
            &format!(
                "[docs]\nusage = [\"demo\"]\nman = \"docs/man.md\"\n\n[presentation]\nbadge = \"D\"\ncolor = \"#8bd5ca\"\nicon = \"{CID}\"\n\n[engine]"
            ),
        );
    let listing = format!(
        "{listing}\n[catalog]\ncategories = [\"utilities\"]\nkeywords = [\"demo\"]\nscreenshots = [{{ cid = \"{CID}\", caption = \"One\" }}, {{ cid = \"{CID}\", caption = \"Two\" }}]\n"
    );
    let listing = Manifest::parse(&listing).expect("a complete listing parses");
    assert_eq!(
        publish_blockers(&listing),
        Vec::<String>::new(),
        "everything mechanical is present"
    );
}

#[test]
fn a_declared_manual_must_exist_and_is_part_of_the_package() {
    let root = fixture();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs/man.md"), b"# Demo\n").unwrap();
    let manifest = MANIFEST.replace(
        "[engine]",
        "[docs]\nusage = [\"demo\"]\nman = \"docs/man.md\"\n\n[engine]",
    );
    fs::write(root.join("spore.toml"), manifest).unwrap();

    let lock = check(&root).unwrap();
    assert!(lock.file("docs/man.md").is_some(), "the manual ships");

    fs::remove_file(root.join("docs/man.md")).unwrap();
    assert!(
        check(&root).is_err(),
        "a manifest that names a manual it does not carry is refused"
    );
}
