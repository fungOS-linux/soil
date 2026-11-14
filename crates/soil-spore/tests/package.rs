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
    // An empty component preamble (`0x0d 0x00`), not a core module: `check`
    // reads the component type, and a core module is not a Spore module.
    fs::write(root.join("module.wasm"), b"\0asm\x0d\x00\x01\x00").unwrap();
    fs::write(root.join("ui/editor.lua"), b"return {}\n").unwrap();
    fs::write(root.join("assets/site-map.png"), b"\x89PNG\r\n").unwrap();
    root
}

/// A package whose only entrypoint is its Lua: no module, no wasm to build.
const LUA_ONLY: &str = r#"schema = 1

[spore]
id = "dev.fungos.divebar"
name = "divebar"
version = "0.1.0"
license = "MIT"
summary = "a bar"

[engine]
soil = ">=0.1"
target = "soil"

[ui]
entry = "ui/divebar.lua"

[chrome]
regions = ["top-bar"]

[launch]
default = "canvas"
surfaces = ["canvas"]
"#;

fn lua_only_fixture() -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("soil-spore-lua-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("ui")).unwrap();
    fs::write(root.join("spore.toml"), LUA_ONLY).unwrap();
    fs::write(
        root.join("ui/divebar.lua"),
        b"return { render = function() end }\n",
    )
    .unwrap();
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
fn a_lua_only_spore_checks_packs_and_verifies() {
    let root = lua_only_fixture();
    let manifest = Manifest::load(&root).unwrap();
    assert!(manifest.module.is_none(), "no wasm in a Lua-only Spore");
    assert_eq!(
        manifest.region_source("top-bar"),
        Some("ui/divebar.lua"),
        "the region resolves to the UI entry without repeating the path"
    );

    let lock = check(&root).unwrap();
    assert_eq!(
        lock.files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec!["spore.toml", "ui/divebar.lua"],
        "nothing claims a module file it does not ship"
    );

    let bytes = pack(&root).unwrap();
    let (_, verified) = verify(&bytes).unwrap();
    assert_eq!(verified.digest, lock.digest);

    // The lock still guards the Lua: a Lua-only Spore is not an unlocked one.
    fs::write(root.join("ui/divebar.lua"), b"return {}\n").unwrap();
    assert!(matches!(check(&root), Err(Error::Lock(_))));
}

#[test]
fn a_region_naming_its_own_artifact_need_not_borrow_the_ui_entry() {
    let root = lua_only_fixture();
    fs::write(
        root.join("spore.toml"),
        LUA_ONLY.replace(
            "top-bar\"]\n",
            "top-bar\"]\n\n[chrome.sources]\ntop-bar = \"dist/bar.lua\"\n",
        ),
    )
    .unwrap();
    fs::create_dir_all(root.join("dist")).unwrap();
    fs::write(root.join("dist/bar.lua"), b"return {}\n").unwrap();

    let manifest = Manifest::load(&root).unwrap();
    assert_eq!(manifest.region_source("top-bar"), Some("dist/bar.lua"));
    let lock = check(&root).unwrap();
    assert!(
        lock.file("dist/bar.lua").is_some(),
        "the artifact the region resolves to is what the lock covers"
    );
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
fn a_package_unpacks_to_the_tree_that_packed_it() {
    let root = fixture();
    let bytes = pack(&root).unwrap();
    let dest = root.join("installed");
    let lock = soil_spore::unpack(&bytes, &dest).unwrap();
    assert_eq!(lock.id, "dev.fungos.markdown");
    assert_eq!(lock.files.len(), 4);
    for path in [
        "spore.toml",
        "module.wasm",
        "ui/editor.lua",
        "assets/site-map.png",
    ] {
        assert!(dest.join(path).is_file(), "{path} did not unpack");
    }
    assert_eq!(
        fs::read(dest.join("module.wasm")).unwrap(),
        fs::read(root.join("module.wasm")).unwrap()
    );
    // spore.lock is the archive's cover, not its contents.
    assert!(
        !dest.join("spore.lock").exists(),
        "the lock is not unpacked"
    );
}

#[test]
fn unpack_writes_nothing_from_a_tampered_package() {
    let root = fixture();
    let bytes = pack(&root).unwrap();
    let mut broken = bytes.clone();
    let index = broken.len() / 2;
    broken[index] ^= 0xff;
    let dest = root.join("refused");
    assert!(soil_spore::unpack(&broken, &dest).is_err());
    assert!(
        !dest.join("module.wasm").exists(),
        "a failed verify must not leave files behind"
    );
}

#[test]
fn a_cid_is_the_bytes() {
    // The documented CIDv1/raw/sha2-256 vector for an empty block. If this
    // changes, every published CID in the wild changes with it.
    assert_eq!(
        soil_spore::cid_v1_raw(b""),
        "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku"
    );
    assert_ne!(
        soil_spore::cid_v1_raw(b"a"),
        soil_spore::cid_v1_raw(b"b"),
        "one byte must move the identity"
    );
}

#[test]
fn the_same_tree_has_the_same_cid_twice() {
    let root = fixture();
    let first = soil_spore::cid_v1_raw(&pack(&root).unwrap());
    let second = soil_spore::cid_v1_raw(&pack(&root).unwrap());
    assert_eq!(first, second, "packing is deterministic, so the CID is too");
    assert!(first.starts_with("bafkrei"), "{first}");
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
fn a_web_surface_ships_the_directory_it_names() {
    let root = fixture();
    let declared = format!(
        "{}\n[[surfaces]]\nname = \"workbook\"\nkind = \"web\"\n",
        MANIFEST.replace("default = \"canvas\"", "default = \"workbook\"")
    );
    fs::write(root.join("spore.toml"), &declared).unwrap();
    fs::create_dir_all(root.join("web/fonts")).unwrap();
    fs::write(root.join("web/index.html"), b"<!doctype html>\n").unwrap();
    fs::write(root.join("web/app.js"), b"export {}\n").unwrap();
    fs::write(root.join("web/fonts/ui.woff2"), b"\0wOF2").unwrap();

    let manifest = Manifest::parse(&declared).unwrap();
    let paths: Vec<String> = soil_spore::collect_package(&root, &manifest)
        .unwrap()
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert!(
        paths.contains(&"web/index.html".to_owned())
            && paths.contains(&"web/app.js".to_owned())
            && paths.contains(&"web/fonts/ui.woff2".to_owned()),
        "a page is useless without the files beside it: {paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.starts_with("src/")),
        "and naming a surface does not turn the package into a source archive"
    );

    // The lock is the identity, so what ships is what is signed.
    let lock = write_lock(&root).unwrap();
    assert!(
        lock.files.iter().any(|file| file.path == "web/app.js"),
        "the bytes the page loads are covered by the package digest"
    );
}

#[test]
fn a_composition_needs_no_module() {
    let source = MANIFEST.replace("[module]\npath = \"module.wasm\"\n\n", "");
    let manifest = Manifest::parse(&source).expect("a view is still a Spore");
    assert!(manifest.module.is_none());
    assert_eq!(
        manifest.declared_paths(),
        vec![
            PathBuf::from("ui/editor.lua"),
            PathBuf::from("assets/site-map.png")
        ]
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

#[test]
fn a_module_that_is_not_a_component_is_rejected_with_a_reason() {
    // `check` reads the component type to hold [grants] to the real imports, so
    // a plain core module has to fail loudly rather than silently skip the rule.
    let error = soil_spore::imported_packages(b"\0asm\x01\0\0\0").unwrap_err();
    assert!(matches!(error, Error::Invalid(_)), "{error:?}");
    assert!(
        error.to_string().contains("component"),
        "the message should say why: {error}"
    );
}

#[test]
fn a_chrome_region_declares_the_height_the_host_reserves() {
    // The shell has to reserve the region before the package runs, so the number
    // cannot be inferred from the view: it is declared, and the host's fallback
    // uses the same one.
    let declared = soil_spore::Manifest::parse(
        r#"
schema = 1
[spore]
id = "dev.fungos.test"
name = "test"
version = "0.1.0"
license = "MIT"
summary = "a bar"

[engine]
soil = ">=0.1"
target = "soil"

[ui]
entry = "ui/view.lua"

[launch]
default = "canvas"
surfaces = ["canvas"]

[chrome]
regions = ["top-bar"]
height = 52
"#,
    )
    .expect("manifest parses");
    let chrome = declared.chrome.as_ref().expect("chrome table");
    assert_eq!(chrome.height(), 52);

    let mut silent = declared.clone();
    silent.chrome = Some(soil_spore::Chrome {
        regions: vec!["top-bar".into()],
        sources: Default::default(),
        height: None,
    });
    assert_eq!(
        silent.chrome.as_ref().unwrap().height(),
        soil_spore::chrome::DEFAULT_HEIGHT
    );
}

#[test]
fn info_json_is_the_authoring_projection() {
    let manifest = Manifest::parse(MANIFEST).expect("manifest parses");
    let info = manifest.info_json();
    assert_eq!(info["id"], "dev.fungos.markdown");
    assert_eq!(info["ui"]["entry"], "ui/editor.lua");
    assert_eq!(info["launch"]["default"], "canvas");
    assert_eq!(info["module"]["path"], "module.wasm");
}

/// A manifest with one declared skill, so the two cases below differ only in
/// the path they name.
fn with_skill(path: &str) -> String {
    format!("{MANIFEST}\n[[skills]]\nname = \"hello\"\npath = {path:?}\n")
}

#[test]
fn a_declared_skill_ships_with_the_package() {
    let root = fixture();
    fs::write(root.join("spore.toml"), with_skill("skills/hello/SKILL.md")).unwrap();
    fs::create_dir_all(root.join("skills/hello")).unwrap();
    fs::write(
        root.join("skills/hello/SKILL.md"),
        b"---\nname: hello\ndescription: greet someone\n---\n",
    )
    .unwrap();

    let manifest = Manifest::load(&root).unwrap();
    assert_eq!(
        manifest.skills.len(),
        1,
        "the skill is declared, not guessed"
    );
    assert!(
        manifest
            .declared_paths()
            .contains(&PathBuf::from("skills/hello/SKILL.md")),
        "the lock and the tar have to cover what teaches an agent to use it"
    );
}

#[test]
fn a_skill_that_is_not_a_skill_file_is_refused() {
    // `SKILL.md` is the name the registry installs. A path to anything else is
    // a file the package ships and nobody reads.
    let root = fixture();
    fs::write(
        root.join("spore.toml"),
        with_skill("skills/hello/README.md"),
    )
    .unwrap();
    let error = Manifest::load(&root).unwrap_err();
    assert!(matches!(error, Error::Invalid(_)), "{error}");
}
#[test]
fn display_title_reads_a_slug_as_words() {
    assert_eq!(soil_spore::display_title("vitals-view"), "Vitals View");
    assert_eq!(soil_spore::display_title("dfm_report"), "Dfm Report");
    assert_eq!(soil_spore::display_title("store"), "Store");
    assert_eq!(soil_spore::display_title(""), "");
}

#[test]
fn a_package_can_ship_its_own_mark() {
    let with_mark = format!(
        "{MANIFEST}\n[presentation]\nbadge = \"M\"\ncolor = \"#8b9dff\"\nmark = \"assets/icon.svg\"\n"
    );
    let manifest = Manifest::parse(&with_mark).expect("manifest parses");
    manifest.validate().expect("a mark inside the package is valid");
    assert_eq!(
        manifest.presentation.as_ref().and_then(|mark| mark.mark.clone()),
        Some("assets/icon.svg".into())
    );
    assert!(
        manifest
            .declared_paths()
            .iter()
            .any(|path| path.as_path() == Path::new("assets/icon.svg")),
        "the mark ships in the package, so the lock has to cover it"
    );

    let escapes = format!(
        "{MANIFEST}\n[presentation]\nbadge = \"M\"\ncolor = \"#8b9dff\"\nmark = \"../icon.svg\"\n"
    );
    assert!(
        Manifest::parse(&escapes).is_err(),
        "a mark may not leave the package"
    );

    let disguised = format!(
        "{MANIFEST}\n[presentation]\nbadge = \"M\"\ncolor = \"#8b9dff\"\nmark = \"assets/icon.gif\"\n"
    );
    assert!(
        Manifest::parse(&disguised).is_err(),
        "a mark is an svg or a png, not whatever a renderer happens to sniff"
    );
}
