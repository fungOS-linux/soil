// Does the renderer stack compile for the browser? Nothing here runs; it only
// has to type-check for wasm32-unknown-unknown, which is the question that
// decides whether the browser host can reuse vello and parley at all.
use taffy::prelude::*;
use vello::kurbo::Rect;
use vello::peniko::{Color, Fill};

pub fn probe(width: u32, height: u32) -> vello::Scene {
    let mut tree: TaffyTree<()> = TaffyTree::new();
    let leaf = tree.new_leaf(Style { size: Size { width: length(10.0_f32), height: auto() }, ..Default::default() }).unwrap();
    tree.compute_layout(leaf, Size { width: AvailableSpace::Definite(width as f32), height: AvailableSpace::Definite(height as f32) }).unwrap();

    let mut fonts = parley::FontContext::new();
    let mut lcx: parley::LayoutContext<[u8; 4]> = parley::LayoutContext::new();
    let mut builder = lcx.ranged_builder(&mut fonts, "probe", 1.0);
    builder.push_default(parley::StyleProperty::FontSize(12.0));
    let mut layout = builder.build("probe");
    layout.break_all_lines(Some(width as f32));

    let mut scene = vello::Scene::new();
    scene.fill(Fill::NonZero, vello::kurbo::Affine::IDENTITY, Color::from_rgb8(0, 0, 0), None,
        &Rect::new(0.0, 0.0, width as f64, height as f64));
    scene
}

/// The same Lua engine the views run on, in wasm. If this links, the browser
/// host can run a view; if it does not, no amount of canvas work matters.
pub fn lua_probe() -> i64 {
    let lua = mlua::Lua::new();
    lua.load("return 40 + 2").eval::<i64>().unwrap_or(-1)
}
