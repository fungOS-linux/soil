//! Rasterize a `soil_scene` into a GPU-owned frame.
//!
//! This is deliberately a host-side renderer: it owns a wgpu device, draws the
//! scene's rects, rasterizes text into one overlay, and returns tightly packed
//! RGBA. A Spore may present that frame as a lease; Canvas still owns the window
//! and the final composite.

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use fontdue::{Font, FontSettings};
use std::io::Read;
use wgpu::util::DeviceExt;

/// Theme values the renderer needs. The scene carries semantic inks, never
/// literal colours; the host resolves them through this table.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub background: [u8; 3],
    pub surface: [u8; 3],
    pub text: [u8; 3],
    pub muted: [u8; 3],
    pub primary: [u8; 3],
    pub success: [u8; 3],
    pub warning: [u8; 3],
    pub danger: [u8; 3],
    pub info: [u8; 3],
    /// Type scale base in logical pixels.
    pub body: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: [12, 15, 20],
            surface: [29, 34, 43],
            text: [232, 236, 244],
            muted: [145, 155, 170],
            primary: [108, 168, 255],
            success: [122, 214, 142],
            warning: [245, 190, 80],
            danger: [255, 122, 122],
            info: [112, 205, 255],
            body: 16.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct TextureVertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

const RECT_SHADER: &str = r#"
struct VertexIn {
  @location(0) pos: vec2<f32>,
  @location(1) color: vec4<f32>,
};
struct VertexOut {
  @builtin(position) position: vec4<f32>,
  @location(0) color: vec4<f32>,
};
@vertex
fn vs_main(in: VertexIn) -> VertexOut {
  return VertexOut(vec4<f32>(in.pos, 0.0, 1.0), in.color);
}
@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
  return in.color;
}
"#;

const TEXT_SHADER: &str = r#"
struct VertexIn {
  @location(0) pos: vec2<f32>,
  @location(1) uv: vec2<f32>,
};
struct VertexOut {
  @builtin(position) position: vec4<f32>,
  @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(in: VertexIn) -> VertexOut {
  return VertexOut(vec4<f32>(in.pos, 0.0, 1.0), in.uv);
}
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
  return textureSample(tex, samp, in.uv);
}
"#;

/// Render one scene at exactly `width x height`.
pub fn render(scene: &soil_scene::Scene, width: u32, height: u32, theme: Theme) -> Result<Frame> {
    anyhow::ensure!(width > 0 && height > 0, "frame needs a non-zero extent");
    anyhow::ensure!(
        width <= 4096 && height <= 4096,
        "frame extent is outside 1..=4096"
    );
    scene.validate().map_err(|error| anyhow::anyhow!(error))?;

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .context("request a GPU adapter")?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("soil-scene-gpu"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        experimental_features: Default::default(),
    }))
    .context("request a GPU device")?;

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("soil-scene-gpu target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let rect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("soil-scene-gpu rects"),
        source: wgpu::ShaderSource::Wgsl(RECT_SHADER.into()),
    });
    let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("soil-scene-gpu rects"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &rect_shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &rect_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("soil-scene-gpu text"),
        source: wgpu::ShaderSource::Wgsl(TEXT_SHADER.into()),
    });
    let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("soil-scene-gpu text"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &text_shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<TextureVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &text_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    let rects = rect_vertices(scene, width, height, theme);
    let rect_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("soil-scene-gpu rect vertices"),
        contents: bytemuck::cast_slice(&rects),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let (overlay, text_vertices) = text_overlay(scene, width, height, theme)?;
    let overlay_texture = device.create_texture_with_data(
        &queue,
        &wgpu::TextureDescriptor {
            label: Some("soil-scene-gpu text overlay"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &overlay,
    );
    let overlay_view = overlay_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("soil-scene-gpu text sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("soil-scene-gpu text bind group"),
        layout: &text_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&overlay_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    let text_buffer = if text_vertices.is_empty() {
        None
    } else {
        Some(
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("soil-scene-gpu text vertices"),
                contents: bytemuck::cast_slice(&text_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
        )
    };

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("soil-scene-gpu encode"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("soil-scene-gpu pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: theme.background[0] as f64 / 255.0,
                        g: theme.background[1] as f64 / 255.0,
                        b: theme.background[2] as f64 / 255.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if !rects.is_empty() {
            pass.set_pipeline(&rect_pipeline);
            pass.set_vertex_buffer(0, rect_buffer.slice(..));
            pass.draw(0..rects.len() as u32, 0..1);
        }
        if let Some(buffer) = &text_buffer {
            pass.set_pipeline(&text_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..text_vertices.len() as u32, 0..1);
        }
    }

    let bytes_per_pixel = 4u32;
    let unpadded = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("soil-scene-gpu readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .context("poll GPU readback")?;
    receiver.recv().context("wait for GPU readback")??;
    let mapped = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        rgba.extend_from_slice(&mapped[start..start + unpadded as usize]);
    }
    drop(mapped);
    readback.unmap();

    Ok(Frame {
        width,
        height,
        rgba,
    })
}

fn ink(theme: Theme, ink: Option<soil_scene::Ink>) -> [u8; 3] {
    match ink.unwrap_or(soil_scene::Ink::Text) {
        soil_scene::Ink::Text => theme.text,
        soil_scene::Ink::Muted => theme.muted,
        soil_scene::Ink::Surface => theme.surface,
        soil_scene::Ink::Background => theme.background,
        soil_scene::Ink::Primary => theme.primary,
        soil_scene::Ink::Success => theme.success,
        soil_scene::Ink::Warning => theme.warning,
        soil_scene::Ink::Danger => theme.danger,
        soil_scene::Ink::Info => theme.info,
    }
}

fn world_bounds(node: &soil_scene::Node, nodes: &[soil_scene::Node]) -> (f32, f32, f32, f32) {
    let (mut x, mut y) = (node.transform.translation[0], node.transform.translation[1]);
    let (mut w, mut h) = (node.transform.scale[0], node.transform.scale[1]);
    let mut parent = node.parent.as_deref();
    let mut guard = 0;
    while let Some(id) = parent {
        guard += 1;
        if guard > 32 {
            break;
        }
        let Some(parent_node) = nodes.iter().find(|candidate| candidate.id == id) else {
            break;
        };
        let (px, py) = (
            parent_node.transform.translation[0],
            parent_node.transform.translation[1],
        );
        x = px + x * parent_node.transform.scale[0];
        y = py + y * parent_node.transform.scale[1];
        w *= parent_node.transform.scale[0];
        h *= parent_node.transform.scale[1];
        parent = parent_node.parent.as_deref();
    }
    (x, y, w, h)
}

fn rect_vertices(scene: &soil_scene::Scene, width: u32, height: u32, theme: Theme) -> Vec<Vertex> {
    let mut out = Vec::new();
    for node in &scene.nodes {
        let soil_scene::Content::Rect(rect) = &node.content else {
            continue;
        };
        let (x, y, w, h) = world_bounds(node, &scene.nodes);
        let (x0, x1) = (x / 4.0, (x + w) / 4.0);
        let (y0, y1) = (y / 4.0, (y + h) / 4.0);
        let color = ink(theme, rect.ink);
        let alpha = rect.opacity.clamp(0.0, 1.0);
        let c = [
            color[0] as f32 / 255.0,
            color[1] as f32 / 255.0,
            color[2] as f32 / 255.0,
            alpha,
        ];
        // `width`/`height` are part of the public contract even though the
        // normalized scene already resolves to clip space.
        let _ = (width, height);
        for (vx, vy) in [(x0, y0), (x1, y0), (x1, y1), (x0, y0), (x1, y1), (x0, y1)] {
            out.push(Vertex {
                pos: [vx, vy],
                color: c,
            });
        }
    }
    out
}

fn text_overlay(
    scene: &soil_scene::Scene,
    width: u32,
    height: u32,
    theme: Theme,
) -> Result<(Vec<u8>, Vec<TextureVertex>)> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let mut vertices = Vec::new();
    let mut font = load_font();
    for node in &scene.nodes {
        let soil_scene::Content::Text(text) = &node.content else {
            continue;
        };
        let Some(font) = font.as_mut() else {
            continue;
        };
        let (x, y, scale, _) = world_bounds(node, &scene.nodes);
        let size = (scale * theme.body * 1.25).max(1.0);
        let screen_x = (x + 4.0) / 8.0 * width as f32;
        let screen_y = (4.0 - y) / 8.0 * height as f32;
        let color = ink(theme, text.ink);
        draw_text(
            &mut rgba,
            width,
            height,
            font,
            text.text.as_str(),
            screen_x,
            screen_y,
            size,
            color,
        );
    }
    // One full-frame textured quad. The overlay is composed on the CPU because
    // text shaping/rasterization is still a host concern; the GPU owns the
    // scene rectangles and the final composite.
    if rgba.iter().any(|byte| *byte != 0) {
        vertices.extend([
            TextureVertex {
                pos: [-1.0, 1.0],
                uv: [0.0, 0.0],
            },
            TextureVertex {
                pos: [1.0, 1.0],
                uv: [1.0, 0.0],
            },
            TextureVertex {
                pos: [1.0, -1.0],
                uv: [1.0, 1.0],
            },
            TextureVertex {
                pos: [-1.0, 1.0],
                uv: [0.0, 0.0],
            },
            TextureVertex {
                pos: [1.0, -1.0],
                uv: [1.0, 1.0],
            },
            TextureVertex {
                pos: [-1.0, -1.0],
                uv: [0.0, 1.0],
            },
        ]);
    }
    Ok((rgba, vertices))
}

fn load_font() -> Option<Font> {
    let candidates = [
        std::env::var("SOIL_SCENE_FONT").ok(),
        Some("/System/Library/Fonts/Supplemental/Arial.ttf".into()),
        Some("/System/Library/Fonts/Supplemental/Arial Unicode.ttf".into()),
        Some("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into()),
        Some("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf".into()),
    ];
    for path in candidates.into_iter().flatten() {
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
            return Some(font);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    font: &mut Font,
    text: &str,
    mut x: f32,
    y: f32,
    size: f32,
    color: [u8; 3],
) {
    let line_height = size * 1.25;
    let start_x = x;
    let mut baseline = y + font
        .horizontal_line_metrics(size)
        .map(|m| m.ascent)
        .unwrap_or(size);
    for ch in text.chars() {
        if ch == '\n' {
            baseline += line_height;
            x = start_x;
            continue;
        }
        let (metrics, bitmap) = font.rasterize(ch, size);
        let gx = x + metrics.xmin as f32;
        let gy = baseline - (metrics.ymin + metrics.height as i32) as f32;
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let coverage = bitmap[row * metrics.width + col];
                if coverage == 0 {
                    continue;
                }
                let px = (gx + col as f32).round() as i32;
                let py = (gy + row as f32).round() as i32;
                if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                    continue;
                }
                let index = ((py as u32 * width + px as u32) * 4) as usize;
                let alpha = coverage as f32 / 255.0;
                blend(rgba, index, color, alpha);
            }
        }
        x += metrics.advance_width;
    }
}

fn blend(rgba: &mut [u8], index: usize, color: [u8; 3], alpha: f32) {
    let dst_a = rgba[index + 3] as f32 / 255.0;
    let src_a = alpha.clamp(0.0, 1.0);
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= f32::EPSILON {
        return;
    }
    for channel in 0..3 {
        let src = color[channel] as f32 / 255.0;
        let dst = rgba[index + channel] as f32 / 255.0;
        let out = (src * src_a + dst * dst_a * (1.0 - src_a)) / out_a;
        rgba[index + channel] = (out * 255.0).round() as u8;
    }
    rgba[index + 3] = (out_a * 255.0).round() as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_node_writes_visible_pixels() {
        let scene = soil_scene::Scene::new(vec![soil_scene::Node {
            id: "title".into(),
            parent: None,
            transform: soil_scene::Transform {
                translation: [-2.0, 1.0, 0.0],
                rotation: [0.0; 3],
                scale: [2.0, 2.0, 1.0],
            },
            content: soil_scene::Content::Text(soil_scene::Text {
                text: "Calendar".into(),
                ink: Some(soil_scene::Ink::Text),
                rgb: None,
                opacity: 1.0,
            }),
        }]);
        let frame = render(&scene, 200, 120, Theme::default()).expect("render");
        let lit = frame
            .rgba
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 50 || pixel[1] > 50 || pixel[2] > 50);
        assert!(lit, "text should leave visible pixels in the frame");
    }

    #[test]
    fn a_scene_produces_an_opaque_frame() {
        let scene = soil_scene::Scene::new(vec![soil_scene::Node {
            id: "ground".into(),
            parent: None,
            transform: soil_scene::Transform {
                translation: [-4.0, -4.0, 0.0],
                rotation: [0.0; 3],
                scale: [8.0, 8.0, 1.0],
            },
            content: soil_scene::Content::Rect(soil_scene::Rect {
                ink: Some(soil_scene::Ink::Surface),
                rgb: None,
                opacity: 1.0,
                radius: 0.0,
            }),
        }]);
        let frame = render(&scene, 32, 24, Theme::default()).expect("render");
        assert_eq!(frame.width, 32);
        assert_eq!(frame.height, 24);
        assert_eq!(frame.rgba.len(), 32 * 24 * 4);
        assert!(frame.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
    }
}
