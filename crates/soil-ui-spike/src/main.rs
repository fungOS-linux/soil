// Spike: the host. Six primitives, everything else is Lua, on PUC Lua 5.4.
//
// Two front ends share one Ui: `--window` runs winit, everything else renders
// offscreen to out.png so the stack can be checked without a display.
mod input;
mod window;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use taffy::prelude::*;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Blob, Color, Fill, Font as UiFont};
use vello::wgpu;
use vello::Scene;

const W: u32 = 640;
const H: u32 = 360;

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Rect { x: f32, y: f32, w: f32, h: f32, r: f32, colour: [u8; 3] },
    Text { value: String, x: f32, y: f32, size: f32, colour: [u8; 3], anchor: String, align: String, font: String, wrap: Option<f32> },
    Hit { id: String, x: f32, y: f32, w: f32, h: f32, action: String },
    /// A scope boundary from `ui.scope`. Commands between two markers belong
    /// to the same key, which is what a fragment cache slices on.
    Group(String),
    Clip { x: f32, y: f32, w: f32, h: f32 },
    ClipEnd,
}

pub struct Fonts {
    fcx: parley::FontContext,
    lcx: parley::LayoutContext<[u8; 4]>,
    faces: HashMap<String, UiFont>,
    families: HashMap<String, String>,
    /// What the shaper cost, so the bench can say whether the cache is working.
    shaping: std::time::Duration,
    shaping_calls: u64,
    cache_hits: u64,
    /// Shaped text, keyed by everything that changes its geometry. Text changes
    /// rarely and shaping is the expensive stage, so this is the first cache.
    cache: HashMap<(String, u32, Option<u32>, String), Rc<parley::Layout<[u8; 4]>>>,
}

impl Fonts {
    fn new() -> Self {
        Self {
            fcx: parley::FontContext::new(),
            lcx: parley::LayoutContext::new(),
            faces: HashMap::new(),
            families: HashMap::new(),
            shaping: std::time::Duration::ZERO,
            shaping_calls: 0,
            cache_hits: 0,
            cache: HashMap::new(),
        }
    }

    fn load(&mut self, name: &str, path: &str) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        // Without its `system` feature parley only knows faces registered here.
        // A face we draw with but parley cannot see lays out zero glyphs, which
        // is exactly the blank screen this comment is here to prevent.
        let families = self.fcx.collection.register_fonts(bytes.clone());
        let family_id = families.first().map(|(id, _)| *id);
        if let Some(id) = family_id {
            if let Some(family) = self.fcx.collection.family_name(id) {
                self.families.insert(name.to_string(), family.to_string());
            }
        }
        self.faces.insert(name.to_string(), UiFont::new(Blob::new(Arc::new(bytes)), 0));
        Ok(())
    }

    fn layout(&mut self, value: &str, size: f32, wrap: Option<f32>, face: &str) -> Rc<parley::Layout<[u8; 4]>> {
        let key = (value.to_string(), size.to_bits(), wrap.map(f32::to_bits), face.to_string());
        if let Some(hit) = self.cache.get(&key) {
            self.cache_hits += 1;
            return hit.clone();
        }
        let started = std::time::Instant::now();
        let layout = self.layout_uncached(value, size, wrap, face);
        self.shaping += started.elapsed();
        self.shaping_calls += 1;
        let layout = Rc::new(layout);
        self.cache.insert(key, layout.clone());
        layout
    }

    fn layout_uncached(&mut self, value: &str, size: f32, wrap: Option<f32>, face: &str) -> parley::Layout<[u8; 4]> {
        let family = self.families.get(face).cloned().unwrap_or_default();
        let mut builder = self.lcx.ranged_builder(&mut self.fcx, value, 1.0);
        builder.push_default(parley::StyleProperty::FontSize(size));
        if !family.is_empty() {
            builder.push_default(parley::StyleProperty::FontStack(parley::FontStack::Single(
                parley::FontFamily::Named(family.into()),
            )));
        }
        let mut layout = builder.build(value);
        layout.break_all_lines(wrap);
        layout
    }

    /// Refuse to start with a face the shaper cannot see. This is the check
    /// that would have caught "commands emitted, glyphs drawn, screen blank".
    fn verify(&mut self, name: &str, path: &str) -> anyhow::Result<()> {
        let layout = self.layout("Mg", 12.0, None, name);
        let glyphs: usize = layout.lines()
            .flat_map(|line| line.items().collect::<Vec<_>>())
            .map(|item| match item {
                parley::PositionedLayoutItem::GlyphRun(run) => run.positioned_glyphs().count(),
                _ => 0,
            })
            .sum();
        if glyphs == 0 {
            anyhow::bail!(
                "face {name:?} ({path}) is registered for drawing but lays out no glyphs; \
                 is it missing from the shaper's collection?"
            );
        }
        Ok(())
    }

    fn face(&self, name: &str) -> Option<&UiFont> {
        self.faces.get(name).or_else(|| self.faces.values().next())
    }
}

fn colour(hex: &str) -> [u8; 3] {
    let h = hex.trim_start_matches('#');
    [u8::from_str_radix(&h[0..2], 16).unwrap_or(0),
     u8::from_str_radix(&h[2..4], 16).unwrap_or(0),
     u8::from_str_radix(&h[4..6], 16).unwrap_or(0)]
}

// Everything the host knows about one frame. The view styles from this; it
// never tracks the pointer, the selection, focus or modifiers itself.
#[derive(Clone)]
pub struct UiState {
    pub hover: String,
    pub pressed: String,
    pub focus: String,
    /// The host owns selection: a click is an action it routes, not a view write.
    pub selected: String,
    /// Scroll offset in logical pixels, owned by the host.
    pub scroll: f32,
    pub pointer: (f32, f32),
    pub mods: input::Mods,
    pub drag: Option<(f32, f32)>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            hover: String::new(),
            pressed: String::new(),
            focus: String::new(),
            selected: "studio".to_string(),
            scroll: 0.0,
            pointer: (0.0, 0.0),
            mods: input::Mods::default(),
            drag: None,
        }
    }
}

/// The Lua side: one view, six primitives, and the commands it produced.
///
/// The engine is PUC Lua 5.4, the same dialect the browser host runs from wasm:
/// `mlua` with vendored C sources here, the wasm build in the browser. One Lua,
/// not two.
pub struct Ui {
    lua: mlua::Lua,
    view: mlua::Table,
    host: mlua::Table,
    fonts: Arc<Mutex<Fonts>>,
    cmds: Arc<Mutex<Vec<Cmd>>>,
    last: Vec<Cmd>,
    previous: Vec<Cmd>,
    /// Built scene for `last`; reused while the commands are unchanged. The
    /// first half of "don't rebuild what didn't change" -- whole-frame, not yet
    /// per command.
    cached: Option<Scene>,
    cached_valid: bool,
}

impl Ui {
    pub fn new() -> anyhow::Result<Self> {
        let cmds = Arc::new(Mutex::new(Vec::<Cmd>::new()));
        let fonts = Arc::new(Mutex::new(Fonts::new()));
        let lua = mlua::Lua::new();

        macro_rules! bind {
            ($name:literal, $f:expr) => {
                lua.globals().set($name, lua.create_function($f).map_err(|e| anyhow::anyhow!("lua: {e}"))?)
                    .map_err(|e| anyhow::anyhow!("lua: {e}"))?;
            };
        }

        {
            let sink = cmds.clone();
            bind!("rect", move |_, (x, y, w, h, r, c): (f32, f32, f32, f32, f32, String)| {
                sink.lock().unwrap().push(Cmd::Rect { x, y, w, h, r, colour: colour(&c) });
                Ok(())
            });
            let sink = cmds.clone();
            bind!("text", move |_, (v, x, y, size, c, a, al, f, wrap): (String, f32, f32, f32, String, Option<String>, Option<String>, Option<String>, Option<f32>)| {
                sink.lock().unwrap().push(Cmd::Text {
                    value: v, x, y, size, colour: colour(&c),
                    anchor: a.unwrap_or_else(|| "middle".into()),
                    align: al.unwrap_or_else(|| "left".into()),
                    font: f.unwrap_or_else(|| "body".into()),
                    wrap,
                });
                Ok(())
            });
            let sink = cmds.clone();
            bind!("group", move |_, key: String| {
                sink.lock().unwrap().push(Cmd::Group(key));
                Ok(())
            });
            let sink = cmds.clone();
            bind!("hit", move |_, (id, x, y, w, h, a): (String, f32, f32, f32, f32, String)| {
                sink.lock().unwrap().push(Cmd::Hit { id, x, y, w, h, action: a });
                Ok(())
            });
            let sink = cmds.clone();
            bind!("clip", move |_, args: mlua::Variadic<f32>| {
                let mut sink = sink.lock().unwrap();
                if args.len() == 4 {
                    sink.push(Cmd::Clip { x: args[0], y: args[1], w: args[2], h: args[3] });
                } else {
                    sink.push(Cmd::ClipEnd);
                }
                Ok(())
            });
            let store = fonts.clone();
            bind!("load_font", move |_, (name, path): (String, String)| {
                store.lock().unwrap().load(&name, &path).map_err(|e| mlua::Error::runtime(e.to_string()))?;
                Ok(())
            });
            let store = fonts.clone();
            // Measurement with the same wrapping the renderer uses, so a view can
            // size a block before it paints it.
            bind!("measure", move |_, (value, size, _face, wrap): (String, f32, Option<String>, Option<f32>)| {
                let mut fonts = store.lock().unwrap();
                let layout = fonts.layout(&value, size, wrap, "body");
                Ok((layout.width(), layout.height()))
            });
        }

        // theme.lua first: the host needs the font path before the first frame.
        let theme: mlua::Table = lua.load(&std::fs::read_to_string("theme.lua")?).eval()
            .map_err(|e| anyhow::anyhow!("theme: {e}"))?;
        let font_path: String = theme.get::<mlua::Table>("font")
            .map_err(|e| anyhow::anyhow!("theme: {e}"))?
            .get("body").map_err(|e| anyhow::anyhow!("theme: {e}"))?;
        {
            let mut store = fonts.lock().unwrap();
            store.load("body", &font_path).map_err(|e| anyhow::anyhow!("font: {e}"))?;
            // A face can be loaded for drawing and invisible to shaping. Ask the
            // shaper directly instead of finding out from a blank screen later.
            store.verify("body", &font_path)?;
        }

        let view_file = std::env::var("SOIL_VIEW").unwrap_or_else(|_| "view.lua".to_string());
        let view: mlua::Table = lua.load(&std::fs::read_to_string(&view_file)?).eval()
            .map_err(|e| anyhow::anyhow!("view: {e}"))?;
        let host = lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        for name in ["rect", "text", "hit", "group", "clip", "measure"] {
            host.set(name, lua.globals().get::<mlua::Value>(name).map_err(|e| anyhow::anyhow!("lua: {e}"))?)
                .map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        }

        Ok(Self {
            lua, view, host, fonts, cmds,
            last: Vec::new(),
            previous: Vec::new(),
            cached: None,
            cached_valid: false,
        })
    }

    /// Lay out, run the view, keep the commands. The host calls this once per
    /// frame, with the input state it decided on.
    pub fn paint(&mut self, width: u32, height: u32, state_in: &UiState) -> anyhow::Result<()> {
        let mut tree: TaffyTree<()> = TaffyTree::new();
        let sidebar = tree.new_leaf(Style { size: Size { width: length(200.0_f32), height: auto() }, ..Default::default() })?;
        let content = tree.new_leaf(Style { flex_grow: 1.0, ..Default::default() })?;
        let root = tree.new_with_children(
            Style { display: Display::Flex, size: Size { width: percent(1.0_f32), height: percent(1.0_f32) }, ..Default::default() },
            &[sidebar, content],
        )?;
        tree.compute_layout(root, Size { width: AvailableSpace::Definite(width as f32), height: AvailableSpace::Definite(height as f32) })?;
        let side = tree.layout(sidebar)?;
        let content_layout = tree.layout(content)?;

        let layout_tbl = self.lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        layout_tbl.set("width", width).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        layout_tbl.set("height", height).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        let side_tbl = self.lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        side_tbl.set("width", side.size.width).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        layout_tbl.set("sidebar", side_tbl).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        let content_tbl = self.lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        content_tbl.set("x", content_layout.location.x).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        content_tbl.set("y", content_layout.location.y).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        layout_tbl.set("content", content_tbl).map_err(|e| anyhow::anyhow!("lua: {e}"))?;

        let state = self.lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        let targets = self.lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        for (i, t) in ["studio", "neo", "acme"].iter().enumerate() {
            targets.set(i + 1, *t).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        }
        state.set("targets", targets).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("selected", state_in.selected.clone()).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("scroll", state_in.scroll).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("hover", state_in.hover.clone()).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("pressed", state_in.pressed.clone()).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("focus", state_in.focus.clone()).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        let pointer = self.lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        pointer.set("x", state_in.pointer.0).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        pointer.set("y", state_in.pointer.1).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("pointer", pointer).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        let mods = self.lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        mods.set("cmd", state_in.mods.cmd).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        mods.set("shift", state_in.mods.shift).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        mods.set("alt", state_in.mods.alt).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        mods.set("ctrl", state_in.mods.ctrl).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("mods", mods).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        if let Some((x, y)) = state_in.drag {
            let drag = self.lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
            drag.set("x", x).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
            drag.set("y", y).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
            state.set("drag", drag).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        }

        self.cmds.lock().unwrap().clear();
        let render: mlua::Function = self.view.get("render").map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        render.call::<()>((self.host.clone(), layout_tbl, state)).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        self.last = self.cmds.lock().unwrap().clone();
        self.cached_valid = self.last == self.previous;
        self.previous = self.last.clone();
        Ok(())
    }

    /// Shaping cost since the last call: (time, misses, hits).
    pub fn take_shaping_stats(&mut self) -> (std::time::Duration, u64, u64) {
        let mut fonts = self.fonts.lock().unwrap();
        let stats = (fonts.shaping, fonts.shaping_calls, fonts.cache_hits);
        fonts.shaping = std::time::Duration::ZERO;
        fonts.shaping_calls = 0;
        fonts.cache_hits = 0;
        stats
    }

    /// The hit regions the last frame registered, in paint order. The host
    /// hit-tests the pointer against these; the view never sees a pointer.
    pub fn regions(&self) -> Vec<input::Region> {
        self.last.iter().filter_map(|cmd| match cmd {
            Cmd::Hit { id, x, y, w, h, .. } => Some(input::Region {
                id: id.clone(),
                rect: input::Rect { x: *x as i32, y: *y as i32, w: *w as i32, h: *h as i32 },
            }),
            _ => None,
        }).collect()
    }

    /// What the view asked for when something was activated. A view registers
    /// an action next to its hit region; the host decides what it means, which
    /// is why a click never mutates view state directly.
    pub fn action(&self, id: &str) -> Option<String> {
        self.last.iter().find_map(|cmd| match cmd {
            Cmd::Hit { id: hit, action, .. } if hit == id => Some(action.clone()),
            _ => None,
        })
    }

    /// The draw commands are the contract between the Lua layer and the
    /// renderer. Two hosts that produce the same pixels but different commands
    /// have still changed something, so `SOIL_TRACE=1` prints them for a diff.
    pub fn trace(&self) {
        if std::env::var_os("SOIL_TRACE").is_none() { return; }
        for cmd in &self.last {
            println!("cmd {cmd:?}");
        }
    }

    pub fn scene(&mut self) -> &Scene {
        if !self.cached_valid || self.cached.is_none() {
            self.cached = Some(self.build_scene());
            self.cached_valid = true;
        }
        self.cached.as_ref().unwrap()
    }

    fn build_scene(&self) -> Scene {
        let mut scene = Scene::new();
        let mut fonts = self.fonts.lock().unwrap();
        for cmd in self.last.iter() {
            match cmd {
                Cmd::Rect { x, y, w, h, r, colour } => {
                    let c = Color::from_rgb8(colour[0], colour[1], colour[2]);
                    if *r > 0.0 {
                        scene.fill(Fill::NonZero, Affine::IDENTITY, c, None,
                            &RoundedRect::new(*x as f64, *y as f64, (x + w) as f64, (y + h) as f64, *r as f64));
                    } else {
                        scene.fill(Fill::NonZero, Affine::IDENTITY, c, None,
                            &Rect::new(*x as f64, *y as f64, (x + w) as f64, (y + h) as f64));
                    }
                }
                Cmd::Clip { x, y, w, h } => scene.push_layer(
                    vello::peniko::BlendMode::default(), 1.0, Affine::IDENTITY,
                    &Rect::new(*x as f64, *y as f64, (x + w) as f64, (y + h) as f64)),
                Cmd::ClipEnd => scene.pop_layer(),
                Cmd::Hit { .. } => {}
                Cmd::Group(_) => {}
                Cmd::Text { value, x, y, size, colour, anchor, align, font, wrap } => {
                    let layout = fonts.layout(value, *size, *wrap, font);
                    let width = layout.width();
                    let mut glyphs = Vec::new();
                    let mut first_baseline = 0.0f32;
                    for line in layout.lines() {
                        for item in line.items() {
                            if let parley::PositionedLayoutItem::GlyphRun(run) = item {
                                let offset = run.offset();
                                let baseline = run.baseline();
                                if first_baseline == 0.0 { first_baseline = baseline; }
                                for glyph in run.positioned_glyphs() {
                                    // y is baseline-relative; the line's baseline
                                    // places it, so wrapped lines stack correctly.
                                    glyphs.push(vello::Glyph { id: glyph.id as u32, x: offset + glyph.x, y: glyph.y });
                                }
                            }
                        }
                    }
                    let left = match align.as_str() {
                        "center" => *x - width / 2.0,
                        "right" => *x - width,
                        _ => *x,
                    };
                    let top = match anchor.as_str() {
                        "baseline" => *y - first_baseline,
                        "middle" => *y - first_baseline + 0.36 * *size,
                        _ => *y,
                    };
                    if glyphs.is_empty() && !value.is_empty() {
                        eprintln!("draw: {value:?} produced no glyphs (face {font:?})");
                    }
                    if let Some(face) = fonts.face(font) {
                        scene.draw_glyphs(face)
                            .font_size(*size)
                            .brush(Color::from_rgb8(colour[0], colour[1], colour[2]))
                            .hint(true)
                            .transform(Affine::translate((left as f64, top as f64)))
                            .draw(Fill::NonZero, glyphs.into_iter());
                    }
                }
            }
        }
        scene
    }
}

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--bench") {
        return bench();
    }

    let mut ui = Ui::new()?;
    if std::env::args().any(|arg| arg == "--window") {
        return window::run(ui);
    }

    // Headless: hit-test a pointer from the environment, then paint with it.
    let pointer = match (std::env::var("PX"), std::env::var("PY")) {
        (Ok(x), Ok(y)) => Some((x.parse::<f32>().unwrap_or(-1.0), y.parse::<f32>().unwrap_or(-1.0))),
        _ => None,
    };
    let mut router = input::Router::new();
    ui.paint(W, H, &UiState::default())?;
    if let Some((px, py)) = pointer {
        let regions = ui.regions();
        router.pointer_moved(&regions, px, py);
        match std::env::var("PBTN").unwrap_or_default().as_str() {
            "1" => { router.pointer_down(&regions, input::Button::Primary, px, py, input::Mods::default()); }
            "2" => {
                router.pointer_down(&regions, input::Button::Primary, px, py, input::Mods::default());
                for intent in router.pointer_up(&regions, px, py, input::Mods::default()) {
                    println!("intent {intent:?}");
                }
            }
            _ => {}
        }
    }
    let state = UiState {
        hover: router.hover().unwrap_or("").to_string(),
        pressed: router.pressed().unwrap_or("").to_string(),
        focus: router.focus().unwrap_or("").to_string(),
        selected: "studio".to_string(),
        scroll: 0.0,
        pointer: router.cursor_position(),
        mods: input::Mods::default(),
        drag: None,
    };
    ui.paint(W, H, &state)?;
    ui.trace();
    println!("pointer {} hover {:?} focus {:?}",
        pointer.map(|p| format!("{:.0},{:.0}", p.0, p.1)).unwrap_or_default(), state.hover, state.focus);
    scene_to_png(&ui.scene(), W, H, "out.png")?;
    println!("wrote out.png");
    Ok(())
}

/// Headless frame budget: what the view costs at N rows, split by stage.
fn bench() -> anyhow::Result<()> {
    fn ms(duration: std::time::Duration) -> f64 {
        duration.as_secs_f64() * 1000.0
    }

    let iters: usize = std::env::var("SOIL_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let rows = std::env::var("SOIL_ROWS").unwrap_or_else(|_| "?".into());
    let virt = std::env::var("SOIL_VIRTUAL").is_ok();
    let mutate = std::env::var("SOIL_MUTATE").is_ok();
    let mut ui = Ui::new()?;
    let mut state = UiState::default();

    // Warm: first frame pays for parsing, module loading and font registration.
    ui.paint(1000, 700, &state)?;
    let _ = ui.scene();
    let _ = ui.take_shaping_stats();

    let mut paint = std::time::Duration::ZERO;
    let mut scene = std::time::Duration::ZERO;
    let mut commands = 0;
    for iteration in 0..iters {
        // The honest case: one thing changes, so a whole-frame cache misses.
        if mutate {
            state.hover = if iteration % 2 == 0 { "row.5".to_string() } else { String::new() };
        }
        let started = std::time::Instant::now();
        ui.paint(1000, 700, &state)?;
        paint += started.elapsed();

        commands = ui.last.len();
        let started = std::time::Instant::now();
        std::hint::black_box(ui.scene());
        scene += started.elapsed();
    }
    let (shaping, misses, hits) = ui.take_shaping_stats();

    println!("rows={rows} virtual={virt} mutate={mutate} iters={iters} commands={commands}");
    println!("  paint  {:>8.2} ms/frame  (taffy + state tables + Lua)", ms(paint) / iters as f64);
    println!("  scene  {:>8.2} ms/frame  (shaping + scene build)", ms(scene) / iters as f64);
    println!(
        "  shape  {:>8.2} ms/frame  ({:.0} shaped, {:.0} cached per frame)",
        ms(shaping) / iters as f64,
        misses as f64 / iters as f64,
        hits as f64 / iters as f64
    );
    Ok(())
}

fn scene_to_png(scene: &Scene, width: u32, height: u32, path: &str) -> anyhow::Result<()> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .ok_or_else(|| anyhow::anyhow!("no adapter"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = vello::Renderer::new(&device, vello::RendererOptions {
        use_cpu: false,
        antialiasing_support: vello::AaSupport::area_only(),
        num_init_threads: std::num::NonZeroUsize::new(1),
        surface_format: None,
    })?;
    renderer.render_to_texture(&device, &queue, scene, &view, &vello::RenderParams {
        base_color: Color::from_rgb8(11, 14, 17),
        width, height,
        antialiasing_method: vello::AaConfig::Area,
    })?;
    let padded = (width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None, size: (padded * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::ImageCopyBuffer { buffer: &buffer, layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(height) } },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        pixels.extend_from_slice(&data[start..start + (width * 4) as usize]);
    }
    image::save_buffer(path, &pixels, width, height, image::ColorType::Rgba8)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real view, under the embedded engine, must paint. The engine is
    /// PUC Lua 5.4 and the gap that matters is a function it does not
    /// have: that shows up here as zero commands, before it shows up as a blank
    /// frame. Needs the font named in `theme.lua`.
    #[test]
    fn view_paints_under_lua54() {
        let mut ui = Ui::new().expect("theme.lua and view.lua load");
        ui.paint(W, H, &UiState::default()).expect("view runs");
        let texts = ui.last.iter().filter(|cmd| matches!(cmd, Cmd::Text { .. })).count();
        assert!(texts > 0, "view emitted no text: did a Lua function come back nil?");
        assert!(!ui.regions().is_empty(), "view emitted no hit regions");
    }
}
