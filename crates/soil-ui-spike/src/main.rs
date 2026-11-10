// Spike: the host. Six primitives, everything else is Lua.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use taffy::prelude::*;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Blob, Color, Fill, Font as UiFont};
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

const W: u32 = 640;
const H: u32 = 360;

#[derive(Clone)]
enum Cmd {
    Rect { x: f32, y: f32, w: f32, h: f32, r: f32, colour: [u8; 3] },
    Text { value: String, x: f32, y: f32, size: f32, colour: [u8; 3], anchor: String, align: String, font: String },
    Hit { id: String, x: f32, y: f32, w: f32, h: f32, action: String },
    Clip { x: f32, y: f32, w: f32, h: f32 },
    ClipEnd,
}

struct Fonts {
    fcx: parley::FontContext,
    lcx: parley::LayoutContext<[u8; 4]>,
    fonts: HashMap<String, UiFont>,
}

impl Fonts {
    fn new() -> Self {
        Self { fcx: parley::FontContext::new(), lcx: parley::LayoutContext::new(), fonts: HashMap::new() }
    }

    fn load(&mut self, name: &str, path: &str) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        self.fonts.insert(name.to_string(), UiFont::new(Blob::new(Arc::new(bytes)), 0));
        Ok(())
    }

    fn layout(&mut self, value: &str, size: f32) -> parley::Layout<[u8; 4]> {
        let mut builder = self.lcx.ranged_builder(&mut self.fcx, value, 1.0);
        builder.push_default(parley::StyleProperty::FontSize(size));
        let mut layout = builder.build(value);
        layout.break_all_lines(None);
        layout
    }

    fn font(&self, name: &str) -> Option<&UiFont> {
        self.fonts.get(name).or_else(|| self.fonts.values().next())
    }
}

fn colour(hex: &str) -> [u8; 3] {
    let h = hex.trim_start_matches('#');
    [u8::from_str_radix(&h[0..2], 16).unwrap_or(0),
     u8::from_str_radix(&h[2..4], 16).unwrap_or(0),
     u8::from_str_radix(&h[4..6], 16).unwrap_or(0)]
}

fn main() -> anyhow::Result<()> {
    // --- taffy: the box split
    let mut tree: TaffyTree<()> = TaffyTree::new();
    let sidebar = tree.new_leaf(Style { size: Size { width: length(200.0), height: auto() }, ..Default::default() })?;
    let content = tree.new_leaf(Style { flex_grow: 1.0, ..Default::default() })?;
    let root = tree.new_with_children(
        Style { display: Display::Flex, size: Size { width: percent(1.0_f32), height: percent(1.0_f32) }, ..Default::default() },
        &[sidebar, content],
    )?;
    tree.compute_layout(root, Size { width: AvailableSpace::Definite(W as f32), height: AvailableSpace::Definite(H as f32) })?;
    let l = tree.layout(sidebar)?;
    let c = tree.layout(content)?;

    let cmds = Arc::new(Mutex::new(Vec::<Cmd>::new()));
    let fonts = Arc::new(Mutex::new(Fonts::new()));
    let lua = mlua::Lua::new();

    macro_rules! bind {
        ($name:literal, $f:expr) => {
            lua.globals().set($name, lua.create_function($f).map_err(|e| anyhow::anyhow!("lua: {e}"))?).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        };
    }

    {
        let sink = cmds.clone();
        bind!("rect", move |_, (x, y, w, h, r, c): (f32, f32, f32, f32, f32, String)| {
            sink.lock().unwrap().push(Cmd::Rect { x, y, w, h, r, colour: colour(&c) });
            Ok(())
        });
        let sink = cmds.clone();
        bind!("text", move |_, (v, x, y, size, c, a, al, f): (String, f32, f32, f32, String, Option<String>, Option<String>, Option<String>)| {
            sink.lock().unwrap().push(Cmd::Text {
                value: v, x, y, size, colour: colour(&c),
                anchor: a.unwrap_or_else(|| "middle".into()),
                align: al.unwrap_or_else(|| "left".into()),
                font: f.unwrap_or_else(|| "body".into()),
            });
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
        bind!("measure", move |_, (value, size, _font): (String, f32, Option<String>)| {
            let mut fonts = store.lock().unwrap();
            let layout = fonts.layout(&value, size);
            Ok(layout.width())
        });
    }

    let theme: mlua::Table = lua.load(&std::fs::read_to_string("theme.lua")?).eval().map_err(|e| anyhow::anyhow!("theme: {e}"))?;
    let font_path: String = theme.get::<mlua::Table>("font").map_err(|e| anyhow::anyhow!("theme: {e}"))?.get("body").map_err(|e| anyhow::anyhow!("theme: {e}"))?;
    fonts.lock().unwrap().load("body", &font_path).map_err(|e| anyhow::anyhow!("font: {e}"))?;

    let view: mlua::Table = lua.load(&std::fs::read_to_string("view.lua")?).eval().map_err(|e| anyhow::anyhow!("view: {e}"))?;
    let host = lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    for name in ["rect", "text", "hit", "clip", "measure"] {
        host.set(name, lua.globals().get::<mlua::Value>(name).map_err(|e| anyhow::anyhow!("lua: {e}"))?).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    }
    let layout_tbl = lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    layout_tbl.set("width", W).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    layout_tbl.set("height", H).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    let side_tbl = lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    side_tbl.set("width", l.size.width).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    layout_tbl.set("sidebar", side_tbl).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    let content_tbl = lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    content_tbl.set("x", c.location.x).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    content_tbl.set("y", c.location.y).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    layout_tbl.set("content", content_tbl).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    let make_state = |hover: &str, pressed: &str| -> anyhow::Result<mlua::Table> {
        let state = lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        let targets = lua.create_table().map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        for (i, t) in ["studio", "neo", "acme"].iter().enumerate() {
            targets.set(i + 1, *t).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        }
        state.set("targets", targets).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("selected", "studio").map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("hover", hover).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        state.set("pressed", pressed).map_err(|e| anyhow::anyhow!("lua: {e}"))?;
        Ok(state)
    };

    let render: mlua::Function = view.get("render").map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    // First pass with no pointer state: the host needs the regions before it can
    // know what the pointer is over. This is the same order a real host works in
    // -- hit-test against the last frame, then paint with the result.
    render.call::<()>((host.clone(), layout_tbl.clone(), make_state("", "")?))
        .map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    let pointer = match (std::env::var("PX"), std::env::var("PY")) {
        (Ok(x), Ok(y)) => Some((x.parse::<f32>().unwrap_or(-1.0), y.parse::<f32>().unwrap_or(-1.0))),
        _ => None,
    };
    let (hover, pressed) = {
        let recorded = cmds.lock().unwrap();
        let mut hover = String::new();
        if let Some((px, py)) = pointer {
            for cmd in recorded.iter().rev() {
                if let Cmd::Hit { id, x, y, w, h, .. } = cmd {
                    if px >= *x && px <= x + w && py >= *y && py <= y + h {
                        hover = id.clone();
                        break;
                    }
                }
            }
        }
        let pressed = if std::env::var("PBTN").is_ok() { hover.clone() } else { String::new() };
        (hover, pressed)
    };
    cmds.lock().unwrap().clear();
    render.call::<()>((host, layout_tbl, make_state(&hover, &pressed)?))
        .map_err(|e| anyhow::anyhow!("lua: {e}"))?;
    println!("pointer {} hover {hover:?}", pointer.map(|p| format!("{:.0},{:.0}", p.0, p.1)).unwrap_or_default());

    let cmds = cmds.lock().unwrap();

    // --- device + draw
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .ok_or_else(|| anyhow::anyhow!("no adapter"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))?;

    let mut scene = Scene::new();
    let mut fonts = fonts.lock().unwrap();
    for cmd in cmds.iter() {
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
            Cmd::Clip { x, y, w, h } => {
                scene.push_layer(vello::peniko::BlendMode::default(), 1.0, Affine::IDENTITY,
                    &Rect::new(*x as f64, *y as f64, (x + w) as f64, (y + h) as f64));
            }
            Cmd::ClipEnd => scene.pop_layer(),
            Cmd::Hit { .. } => {}
            Cmd::Text { value, x, y, size, colour, anchor, align, font } => {
                let layout = fonts.layout(value, *size);
                let width = layout.width();
                let mut glyphs = Vec::new();
                let mut baseline = 0.0f32;
                for line in layout.lines() {
                    for item in line.items() {
                        if let parley::PositionedLayoutItem::GlyphRun(run) = item {
                            let offset = run.offset();
                            baseline = run.baseline();
                            for glyph in run.positioned_glyphs() {
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
                    "baseline" => *y - baseline,
                    "middle" => *y - baseline + 0.36 * *size,
                    _ => *y,
                };
                if let Some(face) = fonts.font(font) {
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
    drop(fonts);
    println!("lua: {} commands", cmds.len());

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    let view_target = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = Renderer::new(&device, RendererOptions {
        use_cpu: false,
        antialiasing_support: AaSupport::area_only(),
        num_init_threads: std::num::NonZeroUsize::new(1),
        surface_format: None,
    })?;
    renderer.render_to_texture(&device, &queue, &scene, &view_target,
        &RenderParams { base_color: Color::from_rgb8(11, 14, 17), width: W, height: H, antialiasing_method: AaConfig::Area })?;

    let padded = (W * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None, size: (padded * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::ImageCopyBuffer { buffer: &buffer, layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(H) } },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H {
        let start = (row * padded) as usize;
        pixels.extend_from_slice(&data[start..start + (W * 4) as usize]);
    }
    image::save_buffer("out.png", &pixels, W, H, image::ColorType::Rgba8)?;
    println!("wrote out.png");
    Ok(())
}
