//! A Lua Spore runs because the host read its manifest, not because a path was
//! guessed. These cases are the ones a package usually loses on: an extent it
//! did not choose, an ink the theme does not define, an id it reused.

use soil_lua::{Spore, Theme};
use soil_scene::Content;
use std::path::PathBuf;

struct Package {
    root: PathBuf,
}

impl Package {
    fn new(name: &str, manifest: &str, files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!("soil-lua-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("spore.toml"), manifest).unwrap();
        for (path, source) in files {
            let path = root.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, source).unwrap();
        }
        Self { root }
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn manifest(target: &str) -> String {
    format!(
        r#"
schema = 1

[spore]
id = "dev.fungos.test"
name = "test"
version = "0.1.0"
license = "MIT"
summary = "a package built by a test"

[engine]
soil = ">=0.1"
target = "{target}"

[ui]
entry = "ui/view.lua"

[launch]
default = "canvas"
surfaces = ["canvas"]
"#
    )
}

fn with_chrome(manifest: &str) -> String {
    format!(
        "{manifest}\n[chrome]\nregions = [\"top-bar\"]\n\n[chrome.sources]\ntop-bar = \"dist/bar.lua\"\n"
    )
}

fn render(package: &Package) -> Result<soil_scene::Scene, soil_lua::Error> {
    Spore::load(&package.root)?.render(
        &serde_json::json!({}),
        &Theme::new(),
        [1200.0, 44.0],
    )
}

#[test]
fn a_document_keeps_the_normalized_space_the_view_drew() {
    // A view draws in -4..4 and the host grants the extent. If the document
    // stored pixels, the same view would render wrong at any other size — which
    // is exactly how a bar that filled 1200x44 filled a quarter of it instead.
    let package = Package::new(
        "extent",
        &manifest("soil"),
        &[(
            "ui/view.lua",
            r#"
return { render = function()
  canvas.rect { id = "bg", translation = { -4, 4 }, scale = { 8, 8 }, ink = "background" }
end }
"#,
        )],
    );
    let spore = Spore::load(&package.root).unwrap();
    let small = spore
        .render(&serde_json::json!({}), &Theme::new(), [1200.0, 44.0])
        .unwrap();
    let large = spore
        .render(&serde_json::json!({}), &Theme::new(), [3840.0, 216.0])
        .unwrap();
    assert_eq!(small, large, "the extent must not rescale the document");
    let node = &small.nodes[0];
    assert_eq!(node.id, "bg");
    assert!(matches!(node.content, Content::Rect(_)));
    assert_eq!(node.transform.translation, [-4.0, 4.0, 0.0]);
    assert_eq!(node.transform.scale, [8.0, 8.0, 1.0]);
}

#[test]
fn a_view_that_paints_at_load_is_drawn_too() {
    let package = Package::new(
        "at-load",
        &manifest("soil"),
        &[(
            "ui/view.lua",
            "canvas.rect { id = 'one', translation = { 0, 0 }, scale = { 1, 1 }, ink = 'text' }\n",
        )],
    );
    let scene = render(&package).unwrap();
    assert_eq!(scene.nodes.len(), 1);
    assert_eq!(scene.nodes[0].id, "one");
}

#[test]
fn a_snapshot_reaches_the_view() {
    let package = Package::new(
        "snapshot",
        &manifest("soil"),
        &[(
            "ui/view.lua",
            r#"
return { render = function(snapshot)
  canvas.text { id = "label", text = snapshot.label, translation = { -3, 4 }, ink = "text" }
end }
"#,
        )],
    );
    let spore = Spore::load(&package.root).unwrap();
    let scene = spore
        .render(
            &serde_json::json!({ "label": "hi" }),
            &Theme::new(),
            [1200.0, 44.0],
        )
        .unwrap();
    match &scene.nodes[0].content {
        Content::Text(text) => assert_eq!(text.text, "hi"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn an_unknown_ink_is_refused_by_name() {
    let package = Package::new(
        "ink",
        &manifest("soil"),
        &[(
            "ui/view.lua",
            "canvas.rect { id = 'bg', translation = { -4, 4 }, scale = { 8, 8 }, ink = 'nonesuch' }\n",
        )],
    );
    let error = render(&package).unwrap_err();
    assert!(error.to_string().contains("nonesuch"), "{error}");
    assert!(error.to_string().contains("bg"), "{error}");
}

#[test]
fn a_duplicate_node_id_is_refused() {
    let package = Package::new(
        "duplicate",
        &manifest("soil"),
        &[(
            "ui/view.lua",
            r#"
canvas.rect { id = "same", translation = { -4, 4 }, scale = { 1, 1 }, ink = "text" }
canvas.rect { id = "same", translation = { 0, 0 }, scale = { 1, 1 }, ink = "text" }
"#,
        )],
    );
    let error = render(&package).unwrap_err();
    assert!(error.to_string().contains("duplicate node id"), "{error}");
}

#[test]
fn a_view_that_draws_nothing_is_refused() {
    let package = Package::new(
        "empty",
        &manifest("soil"),
        &[("ui/view.lua", "return { render = function() end }\n")],
    );
    let error = render(&package).unwrap_err();
    assert!(error.to_string().contains("drew nothing"), "{error}");
}

#[test]
fn a_view_that_errors_reports_its_message() {
    let package = Package::new(
        "throws",
        &manifest("soil"),
        &[(
            "ui/view.lua",
            "return { render = function() error('no workspace') end }\n",
        )],
    );
    let error = render(&package).unwrap_err();
    assert!(error.to_string().contains("no workspace"), "{error}");
    assert!(error.to_string().contains("ui/view.lua"), "{error}");
}

#[test]
fn a_wasm_package_is_refused_by_its_target() {
    let package = Package::new(
        "wasm",
        &manifest("wasm32-wasip2"),
        &[("ui/view.lua", "canvas.rect { id = 'x', ink = 'text' }\n")],
    );
    let error = Spore::load(&package.root).unwrap_err();
    assert!(error.to_string().contains("wasm32-wasip2"), "{error}");
}

#[test]
fn a_region_reads_the_artifact_declared_for_it() {
    let package = Package::new(
        "region",
        &with_chrome(&manifest("soil")),
        &[
            (
                "ui/view.lua",
                "canvas.rect { id = 'ui-entry', ink = 'text' }\n",
            ),
            (
                "dist/bar.lua",
                "canvas.rect { id = 'composed', ink = 'text' }\n",
            ),
        ],
    );
    let spore = Spore::load_region(&package.root, Some("top-bar")).unwrap();
    assert_eq!(spore.entry(), "dist/bar.lua");
    let scene = spore
        .render(&serde_json::json!({}), &Theme::new(), [1200.0, 44.0])
        .unwrap();
    assert_eq!(scene.nodes[0].id, "composed");

    // Without a region the package's own entry is what runs.
    let scene = render(&package).unwrap();
    assert_eq!(scene.nodes[0].id, "ui-entry");
}

#[test]
fn a_region_the_package_does_not_claim_is_refused() {
    let package = Package::new(
        "unclaimed",
        &with_chrome(&manifest("soil")),
        &[(
            "ui/view.lua",
            "canvas.rect { id = 'x', ink = 'text' }\n",
        )],
    );
    let error = Spore::load_region(&package.root, Some("dock")).unwrap_err();
    assert!(error.to_string().contains("dock"), "{error}");
    assert!(error.to_string().contains("top-bar"), "{error}");
}

#[test]
fn a_theme_file_resolves_inks_and_rejects_non_hex() {
    let theme = Theme::from_source(
        "return { name = 'Paper', inks = { accent = '#b5651d', background = '#f5f1e8' } }",
    )
    .unwrap();
    assert_eq!(theme.color("accent"), Some([0xb5, 0x65, 0x1d]));
    assert_eq!(theme.color("nonesuch"), None);

    let error = Theme::from_source("return { inks = { accent = 'orange' } }").unwrap_err();
    assert!(error.to_string().contains("accent"), "{error}");
}

#[test]
fn a_present_is_the_payload_a_host_already_parses() {
    // `soil:canvas.present` and `soil-scene-gpu --scene` both take a `Present`,
    // tagged by `kind`. A bare scene would be refused by either, so this is the
    // interop check rather than a formatting preference.
    let package = Package::new(
        "present",
        &manifest("soil"),
        &[(
            "ui/view.lua",
            "canvas.rect { id = 'x', translation = { -4, 4 }, scale = { 8, 8 }, ink = 'text' }\n",
        )],
    );
    let spore = Spore::load(&package.root).unwrap();
    let present = spore
        .present(&serde_json::json!({}), &Theme::new(), [1200.0, 44.0])
        .unwrap();
    let value = serde_json::to_value(&present).unwrap();
    assert_eq!(value["kind"], "scene");
    assert_eq!(value["schema"], 1);
    assert_eq!(value["nodes"].as_array().map(Vec::len), Some(1));
}
