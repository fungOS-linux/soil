//! Rasterize a `soil_scene` into a GPU-owned frame.
//!
//! This is deliberately a host-side renderer: it owns a wgpu device, draws the
//! scene's rects, rasterizes text into one overlay, and returns tightly packed
//! RGBA. A Spore may present that frame as a lease; Canvas still owns the window
//! and the final composite.

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use fontdue::{Font, FontSettings};
use std::collections::HashMap;
use std::io::Read;

mod gpu;
// The terminal shell. Everything below needs a pty, a VT engine, a window or
// host telemetry, and none of it is reachable from a browser — which is why it
// is a feature and not a module the render core can see.
#[cfg(feature = "term")]
pub mod live;
#[cfg(feature = "term")]
pub mod terminal;
#[cfg(feature = "term")]
pub mod window;

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

impl Theme {
    /// A terminal-oriented palette: slightly purple ground, soft foreground,
    /// and the pastel accents used by modern terminal themes.
    pub fn terminal() -> Self {
        Self {
            background: [17, 17, 27],
            surface: [30, 30, 46],
            text: [205, 214, 244],
            muted: [127, 132, 156],
            primary: [137, 180, 250],
            success: [166, 227, 161],
            warning: [249, 226, 175],
            danger: [243, 139, 168],
            info: [137, 220, 235],
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
    local: [f32; 2],
    extent: [f32; 2],
    radius: f32,
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct TextureVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

const RECT_SHADER: &str = r#"
struct VertexIn {
  @location(0) pos: vec2<f32>,
  @location(1) color: vec4<f32>,
  @location(2) local: vec2<f32>,
  @location(3) extent: vec2<f32>,
  @location(4) radius: f32,
};
struct VertexOut {
  @builtin(position) position: vec4<f32>,
  @location(0) color: vec4<f32>,
  @location(1) local: vec2<f32>,
  @location(2) extent: vec2<f32>,
  @location(3) radius: f32,
};
@vertex
fn vs_main(in: VertexIn) -> VertexOut {
  return VertexOut(vec4<f32>(in.pos, 0.0, 1.0), in.color, in.local, in.extent, in.radius);
}
@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
  // Negative radius marks geometry that is not a rounded quad (lines,
  // triangles, area strips). Those keep their exact vertex coverage.
  if (in.radius < 0.0) {
    return in.color;
  }
  let p = in.local * in.extent;
  let r = clamp(in.radius, 0.0, min(in.extent.x, in.extent.y));
  let q = abs(p) - in.extent + vec2<f32>(r, r);
  let d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
  let aa = max(fwidth(d), 0.00001);
  let coverage = 1.0 - smoothstep(0.0, aa, d);
  return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
"#;

const TEXT_SHADER: &str = r#"
struct VertexIn {
  @location(0) pos: vec2<f32>,
  @location(1) uv: vec2<f32>,
  @location(2) color: vec4<f32>,
};
struct VertexOut {
  @builtin(position) position: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) color: vec4<f32>,
};
@vertex
fn vs_main(in: VertexIn) -> VertexOut {
  return VertexOut(vec4<f32>(in.pos, 0.0, 1.0), in.uv, in.color);
}
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
  let coverage = textureSample(tex, samp, in.uv).r;
  return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
"#;

/// A reusable GPU renderer.
///
/// Creating a wgpu device and its pipelines is a process-lifetime cost, not a
/// frame cost. Hosts that render continuously (a terminal window, a live chart,
/// a presentation loop) should build one `Renderer` and call `render` on it;
/// [`render`] remains the one-shot convenience for callers that truly need it.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    rect_pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,
    font: Option<Font>,
    format: wgpu::TextureFormat,
    samples: u32,
    gpu: gpu::GpuState,
    depth: Option<wgpu::Texture>,
    depth_size: (u32, u32, u32),
    rect_buffer: Option<wgpu::Buffer>,
    rect_capacity: u64,
    rect_scratch: Vec<Vertex>,
    text_buffer: Option<wgpu::Buffer>,
    text_capacity: u64,
    text_scratch: Vec<TextureVertex>,
    atlas: Atlas,
    atlas_texture: wgpu::Texture,
    atlas_bind_group: wgpu::BindGroup,
}

fn ensure_vertex_buffer(
    device: &wgpu::Device,
    buffer: &mut Option<wgpu::Buffer>,
    capacity: &mut u64,
    bytes: usize,
    label: &str,
) {
    let needed = bytes as u64;
    if needed == 0 {
        return;
    }
    if *capacity < needed {
        let size = needed.next_power_of_two().max(256);
        *buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        *capacity = size;
    }
}

fn write_vertices<T: Pod>(queue: &wgpu::Queue, buffer: &Option<wgpu::Buffer>, data: &[T]) {
    if data.is_empty() {
        return;
    }
    if let Some(buffer) = buffer {
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(data));
    }
}

impl Renderer {
    pub fn new() -> Result<Self> {
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
        Self::from_device_sampled(device, queue, wgpu::TextureFormat::Rgba8Unorm, 1)
    }

    /// Build the pipelines for an existing device. Native window hosts use this
    /// so the surface texture and the scene target live on the same device.
    pub fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Result<Self> {
        Self::from_device_sampled(device, queue, format, 1)
    }

    pub fn from_device_sampled(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> Result<Self> {
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
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x4,
                        2 => Float32x2,
                        3 => Float32x2,
                        4 => Float32
                    ],
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
            depth_stencil: Some(gpu::depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
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
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
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
            depth_stencil: Some(gpu::depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("soil-scene-gpu text sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let atlas = Atlas::new();
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("soil-scene-gpu text atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_WIDTH,
                height: ATLAS_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soil-scene-gpu text bind group"),
            layout: &text_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let gpu = gpu::GpuState::new(&device, format, samples)?;

        Ok(Self {
            device,
            queue,
            rect_pipeline,
            text_pipeline,
            font: load_font(),
            format,
            samples,
            gpu,
            depth: None,
            depth_size: (0, 0, 0),
            rect_buffer: None,
            rect_capacity: 0,
            rect_scratch: Vec::new(),
            text_buffer: None,
            text_capacity: 0,
            text_scratch: Vec::new(),
            atlas,
            atlas_texture,
            atlas_bind_group,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn set_font(&mut self, font: Option<Font>) {
        self.font = font;
    }

    /// Draw one scene directly into an existing target view.
    ///
    /// This is the native-window path: no offscreen readback and no second
    /// texture upload. The target must use the format passed to `from_device`.
    pub fn draw_to(
        &mut self,
        scene: &soil_scene::Scene,
        width: u32,
        height: u32,
        theme: Theme,
        target: &wgpu::TextureView,
    ) -> Result<()> {
        self.draw_to_inner(scene, width, height, theme, target, None)
    }

    pub fn draw_to_resolve(
        &mut self,
        scene: &soil_scene::Scene,
        width: u32,
        height: u32,
        theme: Theme,
        target: &wgpu::TextureView,
        resolve: &wgpu::TextureView,
    ) -> Result<()> {
        self.draw_to_inner(scene, width, height, theme, target, Some(resolve))
    }

    fn ensure_depth(&mut self, width: u32, height: u32) -> wgpu::TextureView {
        let key = (width, height, self.samples);
        if self.depth.is_none() || self.depth_size != key {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("soil-scene-gpu depth"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: self.samples,
                dimension: wgpu::TextureDimension::D2,
                format: gpu::DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth = Some(texture);
            self.depth_size = key;
        }
        self.depth
            .as_ref()
            .expect("depth texture")
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn draw_to_inner(
        &mut self,
        scene: &soil_scene::Scene,
        width: u32,
        height: u32,
        theme: Theme,
        target: &wgpu::TextureView,
        resolve: Option<&wgpu::TextureView>,
    ) -> Result<()> {
        anyhow::ensure!(width > 0 && height > 0, "frame needs a non-zero extent");
        anyhow::ensure!(
            width <= 4096 && height <= 4096,
            "frame extent is outside 1..=4096"
        );
        scene.validate().map_err(|error| anyhow::anyhow!(error))?;
        let depth = self.ensure_depth(width, height);
        let device = &self.device;
        let queue = &self.queue;
        let gpu_draws = self
            .gpu
            .prepare(device, queue, scene, width, height, theme)?;

        self.rect_scratch.clear();
        rect_vertices(&mut self.rect_scratch, scene, width, height, theme);
        ensure_vertex_buffer(
            device,
            &mut self.rect_buffer,
            &mut self.rect_capacity,
            self.rect_scratch.len() * std::mem::size_of::<Vertex>(),
            "soil-scene-gpu rect vertices",
        );
        write_vertices(queue, &self.rect_buffer, &self.rect_scratch);

        self.text_scratch.clear();
        text_vertices(
            &mut self.text_scratch,
            scene,
            width,
            height,
            theme,
            &mut self.atlas,
            self.font.as_mut(),
        );
        if self.atlas.dirty {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.atlas.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ATLAS_WIDTH),
                    rows_per_image: Some(ATLAS_HEIGHT),
                },
                wgpu::Extent3d {
                    width: ATLAS_WIDTH,
                    height: ATLAS_HEIGHT,
                    depth_or_array_layers: 1,
                },
            );
            self.atlas.dirty = false;
        }
        ensure_vertex_buffer(
            device,
            &mut self.text_buffer,
            &mut self.text_capacity,
            self.text_scratch.len() * std::mem::size_of::<TextureVertex>(),
            "soil-scene-gpu text vertices",
        );
        write_vertices(queue, &self.text_buffer, &self.text_scratch);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("soil-scene-gpu encode"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("soil-scene-gpu pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: theme.background[0] as f64 / 255.0,
                            g: theme.background[1] as f64 / 255.0,
                            b: theme.background[2] as f64 / 255.0,
                            a: 1.0,
                        }),
                        store: if resolve.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !self.rect_scratch.is_empty()
                && let Some(buffer) = &self.rect_buffer
            {
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..self.rect_scratch.len() as u32, 0..1);
            }
            self.gpu.draw(&mut pass, &gpu_draws);
            if !self.text_scratch.is_empty()
                && let Some(buffer) = &self.text_buffer
            {
                pass.set_pipeline(&self.text_pipeline);
                pass.set_bind_group(0, &self.atlas_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..self.text_scratch.len() as u32, 0..1);
            }
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }

    /// Render one scene into a CPU-readable frame.
    pub fn render(
        &mut self,
        scene: &soil_scene::Scene,
        width: u32,
        height: u32,
        theme: Theme,
    ) -> Result<Frame> {
        anyhow::ensure!(width > 0 && height > 0, "frame needs a non-zero extent");
        anyhow::ensure!(
            width <= 4096 && height <= 4096,
            "frame extent is outside 1..=4096"
        );
        scene.validate().map_err(|error| anyhow::anyhow!(error))?;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("soil-scene-gpu target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.draw_to(scene, width, height, theme, &view)?;
        let device = &self.device;
        let queue = &self.queue;

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
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("soil-scene-gpu readback"),
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
}

/// Render one scene with a fresh renderer.
///
/// Prefer [`Renderer`] for repeated frames.
pub fn render(scene: &soil_scene::Scene, width: u32, height: u32, theme: Theme) -> Result<Frame> {
    let mut renderer = Renderer::new()?;
    renderer.render(scene, width, height, theme)
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}

pub(crate) fn rgb_or_ink(
    theme: Theme,
    rgb: Option<[u8; 3]>,
    semantic: Option<soil_scene::Ink>,
) -> [u8; 3] {
    rgb.unwrap_or_else(|| ink(theme, semantic))
}

pub(crate) fn ink(theme: Theme, ink: Option<soil_scene::Ink>) -> [u8; 3] {
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

pub(crate) fn world_bounds(
    node: &soil_scene::Node,
    nodes: &[soil_scene::Node],
) -> (f32, f32, f32, f32) {
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

fn heat_color(_theme: Theme, t: f32) -> [u8; 3] {
    // Viridis-like ramp: density fields start dark, then become cyan/green/
    // yellow. A semantic primary at t=0 turns every occupied cell into a slab
    // of blue and hides the structure a density map is meant to show.
    let stops = [
        (0.0, [35, 18, 66]),
        (0.25, [64, 57, 132]),
        (0.5, [0, 125, 160]),
        (0.75, [40, 200, 120]),
        (1.0, [255, 225, 70]),
    ];
    let t = t.clamp(0.0, 1.0);
    for pair in stops.windows(2) {
        let (a, ca) = pair[0];
        let (b, cb) = pair[1];
        if t <= b {
            let u = ((t - a) / (b - a)).clamp(0.0, 1.0);
            return [
                (ca[0] as f32 + (cb[0] as f32 - ca[0] as f32) * u) as u8,
                (ca[1] as f32 + (cb[1] as f32 - ca[1] as f32) * u) as u8,
                (ca[2] as f32 + (cb[2] as f32 - ca[2] as f32) * u) as u8,
            ];
        }
    }
    stops[stops.len() - 1].1
}

fn rect_vertices(
    out: &mut Vec<Vertex>,
    scene: &soil_scene::Scene,
    width: u32,
    height: u32,
    theme: Theme,
) {
    let _ = (width, height);
    for node in &scene.nodes {
        let (x, y, w, h) = world_bounds(node, &scene.nodes);
        match &node.content {
            soil_scene::Content::Rect(rect) => {
                let color = rgb_or_ink(theme, rect.rgb, rect.ink);
                // Scene translation is the top-left in a y-up space; scale is
                // positive down, so the quad spans `y - h .. y`.
                push_quad(out, x, y - h, x + w, y, color, rect.opacity, rect.radius);
            }
            soil_scene::Content::Area(area) => {
                let color = rgb_or_ink(theme, area.rgb, area.ink);
                let alpha = area.opacity.clamp(0.0, 1.0);
                let base_alpha = area.base_opacity.unwrap_or(alpha).clamp(0.0, 1.0);
                for pair in area.points.windows(2) {
                    let (x1, y1) = (x + pair[0][0] * w, y + pair[0][1] * h);
                    let (x2, y2) = (x + pair[1][0] * w, y + pair[1][1] * h);
                    let base = y + area.baseline * h;
                    push_segment_gradient(
                        out,
                        [x1, y1],
                        [x2, y2],
                        [x2, base],
                        [x1, base],
                        color,
                        alpha,
                        base_alpha,
                    );
                }
            }
            soil_scene::Content::Band(band) => {
                let color = rgb_or_ink(theme, band.rgb, band.ink);
                let alpha = band.opacity.clamp(0.0, 1.0);
                for index in 0..band.upper.len().saturating_sub(1) {
                    let upper0 = band.upper[index];
                    let upper1 = band.upper[index + 1];
                    let lower0 = band.lower[index];
                    let lower1 = band.lower[index + 1];
                    let u0 = [x + upper0[0] * w, y + upper0[1] * h];
                    let u1 = [x + upper1[0] * w, y + upper1[1] * h];
                    let l1 = [x + lower1[0] * w, y + lower1[1] * h];
                    let l0 = [x + lower0[0] * w, y + lower0[1] * h];
                    push_segment(out, u0, u1, l1, l0, color, alpha);
                }
            }
            soil_scene::Content::Points(points) => {
                let color = rgb_or_ink(theme, points.rgb, points.ink);
                let alpha = points.opacity.clamp(0.0, 1.0);
                let half_w = points.size[0] * 0.5 * w;
                let half_h = points.size[1] * 0.5 * h;
                let radius = if points.shape == soil_scene::PointShape::Circle {
                    half_w.min(half_h)
                } else {
                    0.0
                };
                for point in &points.points {
                    let px = x + point[0] * w;
                    let py = y + point[1] * h;
                    push_quad(
                        out,
                        px - half_w,
                        py - half_h,
                        px + half_w,
                        py + half_h,
                        color,
                        alpha,
                        radius,
                    );
                }
            }
            soil_scene::Content::Heatmap(heatmap) => {
                let cols = heatmap.cols as usize;
                let rows = heatmap.rows as usize;
                let cell_w = w / cols as f32;
                let cell_h = h / rows as f32;
                let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                for value in &heatmap.values {
                    lo = lo.min(*value);
                    hi = hi.max(*value);
                }
                let lo = heatmap.min.unwrap_or(lo);
                let hi = heatmap.max.unwrap_or(hi);
                let span = if (hi - lo).abs() <= f32::EPSILON {
                    1.0
                } else {
                    hi - lo
                };
                for row in 0..rows {
                    for col in 0..cols {
                        let value = heatmap.values[row * cols + col];
                        if heatmap.zero_transparent && value <= 0.0 {
                            continue;
                        }
                        let t = ((value - lo) / span).clamp(0.0, 1.0);
                        let color = heat_color(theme, t);
                        let x0 = x + col as f32 * cell_w;
                        let top = y - row as f32 * cell_h;
                        push_quad(
                            out,
                            x0,
                            top - cell_h - 0.5,
                            x0 + cell_w + 0.5,
                            top,
                            color,
                            heatmap.opacity.clamp(0.0, 1.0),
                            0.0,
                        );
                    }
                }
            }
            soil_scene::Content::Candles(candles) => {
                let alpha = candles.opacity.clamp(0.0, 1.0);
                let up = candles.up_rgb.unwrap_or_else(|| ink(theme, candles.up_ink));
                let down = candles
                    .down_rgb
                    .unwrap_or_else(|| ink(theme, candles.down_ink));
                let body = candles.width * w;
                let pixel_x = 8.0 / width.max(1) as f32;
                let pixel_y = 8.0 / height.max(1) as f32;
                let wick = (body * 0.12).max(pixel_x);
                for candle in &candles.candles {
                    let cx = x + candle.x * w;
                    let high = y + candle.high * h;
                    let low = y + candle.low * h;
                    let open = y + candle.open * h;
                    let close = y + candle.close * h;
                    let color = if candle.close >= candle.open {
                        up
                    } else {
                        down
                    };
                    push_quad(
                        out,
                        cx - wick * 0.5,
                        high.min(low),
                        cx + wick * 0.5,
                        high.max(low),
                        color,
                        alpha,
                        0.0,
                    );
                    let top = open.min(close);
                    let bottom = open.max(close);
                    push_quad(
                        out,
                        cx - body * 0.5,
                        top,
                        cx + body * 0.5,
                        bottom.max(top + pixel_y),
                        color,
                        alpha,
                        0.0,
                    );
                }
            }
            soil_scene::Content::Polyline(polyline) => {
                let color = rgb_or_ink(theme, polyline.rgb, polyline.ink);
                let color2 = polyline.rgb2;
                let alpha = polyline.opacity.clamp(0.0, 1.0);
                let half = polyline.width * 0.5;
                let points: Vec<[f32; 2]> = polyline
                    .points
                    .iter()
                    .map(|point| [x + point[0] * w, y + point[1] * h])
                    .collect();
                let min_x = points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
                let max_x = points
                    .iter()
                    .map(|p| p[0])
                    .fold(f32::NEG_INFINITY, f32::max);
                let span = (max_x - min_x).max(0.0001);
                let color_at = |px: f32| {
                    color2
                        .map(|end| lerp_rgb(color, end, ((px - min_x) / span).clamp(0.0, 1.0)))
                        .unwrap_or(color)
                };
                for pair in points.windows(2) {
                    let [x1, y1] = pair[0];
                    let [x2, y2] = pair[1];
                    let dx = x2 - x1;
                    let dy = y2 - y1;
                    let length = (dx * dx + dy * dy).sqrt();
                    if length <= f32::EPSILON {
                        continue;
                    }
                    let nx = -dy / length * half;
                    let ny = dx / length * half;
                    let segment_color = color_at((x1 + x2) * 0.5);
                    push_segment(
                        out,
                        [x1 + nx, y1 + ny],
                        [x2 + nx, y2 + ny],
                        [x2 - nx, y2 - ny],
                        [x1 - nx, y1 - ny],
                        segment_color,
                        alpha,
                    );
                }
                for index in 1..points.len().saturating_sub(1) {
                    push_join(
                        out,
                        points[index - 1],
                        points[index],
                        points[index + 1],
                        half,
                        polyline.join,
                        polyline.miter_limit,
                        color_at(points[index][0]),
                        alpha,
                    );
                }
            }
            _ => {}
        }
    }
}

fn normalize(v: [f32; 2]) -> Option<[f32; 2]> {
    let length = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if length <= f32::EPSILON {
        None
    } else {
        Some([v[0] / length, v[1] / length])
    }
}

fn push_triangle(
    out: &mut Vec<Vertex>,
    a: [f32; 2],
    b: [f32; 2],
    c: [f32; 2],
    color: [u8; 3],
    alpha: f32,
) {
    let rgba = [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        alpha.clamp(0.0, 1.0),
    ];
    for point in [a, b, c] {
        out.push(Vertex {
            pos: [point[0] / 4.0, point[1] / 4.0],
            color: rgba,
            local: [0.0, 0.0],
            extent: [0.0, 0.0],
            radius: -1.0,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn push_join(
    out: &mut Vec<Vertex>,
    previous: [f32; 2],
    vertex: [f32; 2],
    next: [f32; 2],
    half: f32,
    join: soil_scene::LineJoin,
    miter_limit: f32,
    color: [u8; 3],
    alpha: f32,
) {
    let Some(d0) = normalize([vertex[0] - previous[0], vertex[1] - previous[1]]) else {
        return;
    };
    let Some(d1) = normalize([next[0] - vertex[0], next[1] - vertex[1]]) else {
        return;
    };
    let cross = d0[0] * d1[1] - d0[1] * d1[0];
    if cross.abs() <= 0.0001 {
        return;
    }
    // Left-hand normal of each segment. The outer side of the turn is the
    // opposite sign to the turn direction.
    let side = if cross > 0.0 { -1.0 } else { 1.0 };
    let n0 = [-d0[1] * side, d0[0] * side];
    let n1 = [-d1[1] * side, d1[0] * side];
    let p0 = [vertex[0] + n0[0] * half, vertex[1] + n0[1] * half];
    let p1 = [vertex[0] + n1[0] * half, vertex[1] + n1[1] * half];

    match join {
        soil_scene::LineJoin::Round => {
            let start = n0[1].atan2(n0[0]);
            let end = n1[1].atan2(n1[0]);
            let mut delta = end - start;
            while delta > std::f32::consts::PI {
                delta -= std::f32::consts::TAU;
            }
            while delta < -std::f32::consts::PI {
                delta += std::f32::consts::TAU;
            }
            let steps = ((delta.abs() * half / 1.5).ceil() as usize).clamp(2, 32);
            let mut last = p0;
            for step in 1..=steps {
                let angle = start + delta * step as f32 / steps as f32;
                let point = [
                    vertex[0] + angle.cos() * half,
                    vertex[1] + angle.sin() * half,
                ];
                push_triangle(out, vertex, last, point, color, alpha);
                last = point;
            }
            let _ = p1;
        }
        soil_scene::LineJoin::Bevel => {
            push_triangle(out, vertex, p0, p1, color, alpha);
        }
        soil_scene::LineJoin::Miter => {
            let Some(direction) = normalize([n0[0] + n1[0], n0[1] + n1[1]]) else {
                push_triangle(out, vertex, p0, p1, color, alpha);
                return;
            };
            let cosine = direction[0] * n0[0] + direction[1] * n0[1];
            let miter_length = if cosine > 0.0001 {
                half / cosine
            } else {
                f32::INFINITY
            };
            if miter_length <= miter_limit * half {
                let tip = [
                    vertex[0] + direction[0] * miter_length,
                    vertex[1] + direction[1] * miter_length,
                ];
                push_triangle(out, vertex, p0, tip, color, alpha);
                push_triangle(out, vertex, tip, p1, color, alpha);
            } else {
                push_triangle(out, vertex, p0, p1, color, alpha);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_quad(
    out: &mut Vec<Vertex>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: [u8; 3],
    alpha: f32,
    radius: f32,
) {
    let alpha = alpha.clamp(0.0, 1.0);
    let c = [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        alpha,
    ];
    let extent = [(x1 - x0) / 8.0, (y1 - y0) / 8.0];
    for (vx, vy, lx, ly) in [
        (x0 / 4.0, y0 / 4.0, -1.0, -1.0),
        (x1 / 4.0, y0 / 4.0, 1.0, -1.0),
        (x1 / 4.0, y1 / 4.0, 1.0, 1.0),
        (x0 / 4.0, y0 / 4.0, -1.0, -1.0),
        (x1 / 4.0, y1 / 4.0, 1.0, 1.0),
        (x0 / 4.0, y1 / 4.0, -1.0, 1.0),
    ] {
        out.push(Vertex {
            pos: [vx, vy],
            color: c,
            local: [lx, ly],
            extent,
            radius: radius / 4.0,
        });
    }
}

fn push_segment(
    out: &mut Vec<Vertex>,
    a: [f32; 2],
    b: [f32; 2],
    c: [f32; 2],
    d: [f32; 2],
    color: [u8; 3],
    alpha: f32,
) {
    push_segment_gradient(out, a, b, c, d, color, alpha, alpha);
}

#[allow(clippy::too_many_arguments)]
fn push_segment_gradient(
    out: &mut Vec<Vertex>,
    a: [f32; 2],
    b: [f32; 2],
    c: [f32; 2],
    d: [f32; 2],
    color: [u8; 3],
    top_alpha: f32,
    base_alpha: f32,
) {
    let rgb = [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
    ];
    let top = [rgb[0], rgb[1], rgb[2], top_alpha];
    let base = [rgb[0], rgb[1], rgb[2], base_alpha];
    for (point, rgba) in [
        (a, top),
        (b, top),
        (c, base),
        (a, top),
        (c, base),
        (d, base),
    ] {
        out.push(Vertex {
            pos: [point[0] / 4.0, point[1] / 4.0],
            color: rgba,
            local: [0.0, 0.0],
            extent: [0.0, 0.0],
            radius: -1.0,
        });
    }
}

const ATLAS_WIDTH: u32 = 1024;
const ATLAS_HEIGHT: u32 = 1024;

struct Glyph {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    advance: f32,
}

struct Atlas {
    data: Vec<u8>,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    glyphs: HashMap<(char, u32), Option<Glyph>>,
    dirty: bool,
}

impl Atlas {
    fn new() -> Self {
        Self {
            data: vec![0u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize],
            cursor_x: 1,
            cursor_y: 1,
            row_height: 0,
            glyphs: HashMap::new(),
            dirty: true,
        }
    }

    fn glyph(&mut self, font: &mut Font, ch: char, size: f32) -> Option<Glyph> {
        let key = (ch, size.round().max(1.0) as u32);
        if let Some(glyph) = self.glyphs.get(&key) {
            return glyph.as_ref().map(|glyph| Glyph {
                x: glyph.x,
                y: glyph.y,
                width: glyph.width,
                height: glyph.height,
                left: glyph.left,
                top: glyph.top,
                advance: glyph.advance,
            });
        }
        self.dirty = true;
        let (metrics, bitmap) = font.rasterize(ch, size);
        let glyph = if metrics.width == 0 || metrics.height == 0 {
            Some(Glyph {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                left: metrics.xmin,
                top: metrics.height as i32 + metrics.ymin,
                advance: metrics.advance_width,
            })
        } else {
            let width = metrics.width as u32;
            let height = metrics.height as u32;
            if self.cursor_x + width + 1 > ATLAS_WIDTH {
                self.cursor_x = 1;
                self.cursor_y += self.row_height + 1;
                self.row_height = 0;
            }
            if self.cursor_y + height + 1 > ATLAS_HEIGHT {
                self.glyphs.insert(key, None);
                return None;
            }
            let x = self.cursor_x;
            let y = self.cursor_y;
            for row in 0..height {
                let src = (row * width) as usize;
                let dst = ((y + row) * ATLAS_WIDTH + x) as usize;
                self.data[dst..dst + width as usize]
                    .copy_from_slice(&bitmap[src..src + width as usize]);
            }
            self.cursor_x += width + 1;
            self.row_height = self.row_height.max(height);
            Some(Glyph {
                x,
                y,
                width,
                height,
                left: metrics.xmin,
                top: metrics.height as i32 + metrics.ymin,
                advance: metrics.advance_width,
            })
        };
        self.glyphs.insert(
            key,
            glyph.as_ref().map(|glyph| Glyph {
                x: glyph.x,
                y: glyph.y,
                width: glyph.width,
                height: glyph.height,
                left: glyph.left,
                top: glyph.top,
                advance: glyph.advance,
            }),
        );
        glyph
    }
}

fn text_vertices(
    out: &mut Vec<TextureVertex>,
    scene: &soil_scene::Scene,
    width: u32,
    height: u32,
    theme: Theme,
    atlas: &mut Atlas,
    font: Option<&mut Font>,
) {
    let mut font = font;
    for node in &scene.nodes {
        let soil_scene::Content::Text(text) = &node.content else {
            continue;
        };
        let Some(font) = font.as_mut() else {
            continue;
        };
        let font: &mut Font = font;
        let (x, y, scale, _) = world_bounds(node, &scene.nodes);
        let size = (scale * theme.body * 1.25).max(1.0);
        let screen_x = (x + 4.0) / 8.0 * width as f32;
        let screen_y = (4.0 - y) / 8.0 * height as f32;
        let color = text.rgb.unwrap_or_else(|| ink(theme, text.ink));
        push_text_quads(
            atlas,
            out,
            font,
            text.text.as_str(),
            screen_x,
            screen_y,
            size,
            color,
            width,
            height,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_text_quads(
    atlas: &mut Atlas,
    vertices: &mut Vec<TextureVertex>,
    font: &mut Font,
    text: &str,
    mut x: f32,
    y: f32,
    size: f32,
    color: [u8; 3],
    width: u32,
    height: u32,
) {
    let line_height = size * 1.25;
    let start_x = x;
    let mut baseline = y + font
        .horizontal_line_metrics(size)
        .map(|m| m.ascent)
        .unwrap_or(size);
    let rgba = [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        1.0,
    ];
    for ch in text.chars() {
        if ch == '\n' {
            baseline += line_height;
            x = start_x;
            continue;
        }
        let Some(glyph) = atlas.glyph(font, ch, size) else {
            x += size * 0.5;
            continue;
        };
        if glyph.width > 0 && glyph.height > 0 {
            let x0 = x + glyph.left as f32;
            let y0 = baseline - glyph.top as f32;
            let x1 = x0 + glyph.width as f32;
            let y1 = y0 + glyph.height as f32;
            let (u0, v0) = (
                glyph.x as f32 / ATLAS_WIDTH as f32,
                glyph.y as f32 / ATLAS_HEIGHT as f32,
            );
            let (u1, v1) = (
                (glyph.x + glyph.width) as f32 / ATLAS_WIDTH as f32,
                (glyph.y + glyph.height) as f32 / ATLAS_HEIGHT as f32,
            );
            let (nx0, nx1) = (x0 / width as f32 * 2.0 - 1.0, x1 / width as f32 * 2.0 - 1.0);
            let (ny0, ny1) = (
                1.0 - y0 / height as f32 * 2.0,
                1.0 - y1 / height as f32 * 2.0,
            );
            for (px, py, u, v) in [
                (nx0, ny0, u0, v0),
                (nx1, ny0, u1, v0),
                (nx1, ny1, u1, v1),
                (nx0, ny0, u0, v0),
                (nx1, ny1, u1, v1),
                (nx0, ny1, u0, v1),
            ] {
                vertices.push(TextureVertex {
                    pos: [px, py],
                    uv: [u, v],
                    color: rgba,
                });
            }
        }
        x += glyph.advance;
    }
}

fn load_font_path(candidates: impl IntoIterator<Item = String>) -> Option<Font> {
    for path in candidates {
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

fn load_font() -> Option<Font> {
    load_font_path(
        [
            std::env::var("SOIL_SCENE_FONT").ok(),
            Some("/System/Library/Fonts/SFNS.ttf".into()),
            Some("/System/Library/Fonts/Supplemental/Avenir Next.ttc".into()),
            Some("/System/Library/Fonts/Supplemental/Arial.ttf".into()),
            Some("/System/Library/Fonts/Supplemental/Arial Unicode.ttf".into()),
            Some("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into()),
            Some("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf".into()),
        ]
        .into_iter()
        .flatten(),
    )
}

/// Load the terminal monospace face. Environment override wins, then the
/// platform monospace, then a Nerd Font for prompt glyphs.
pub fn load_mono_font() -> Option<Font> {
    let home = std::env::var("HOME").unwrap_or_default();
    load_font_path(
        [
            std::env::var("SOIL_TERMINAL_FONT").ok(),
            Some("/System/Library/Fonts/SFNSMono.ttf".into()),
            Some("/System/Library/Fonts/Menlo.ttc".into()),
            Some(format!(
                "{home}/Library/Fonts/Fira Code Regular Nerd Font Complete Mono.otf"
            )),
            Some(format!(
                "{home}/Library/Fonts/JetBrainsMonoNerdFont-Regular.ttf"
            )),
            Some("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into()),
            Some("/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf".into()),
        ]
        .into_iter()
        .flatten(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_node_writes_visible_pixels() {
        let scene = soil_scene::Scene::new(vec![soil_scene::Node {
            id: "title".into(),
            parent: None,
            transition_ms: None,
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
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[0] > 50 || pixel[1] > 50 || pixel[2] > 50);
        assert!(lit, "text should leave visible pixels in the frame");
    }

    #[test]
    fn repeated_frames_reuse_the_same_renderer() {
        let scene = soil_scene::Scene::new(vec![soil_scene::Node {
            id: "title".into(),
            parent: None,
            transition_ms: None,
            transform: soil_scene::Transform {
                translation: [-2.0, 1.0, 0.0],
                rotation: [0.0; 3],
                scale: [2.0, 2.0, 1.0],
            },
            content: soil_scene::Content::Text(soil_scene::Text {
                text: "Reuse".into(),
                ink: Some(soil_scene::Ink::Text),
                rgb: None,
                opacity: 1.0,
            }),
        }]);
        let mut renderer = Renderer::new().expect("renderer");
        let first = renderer
            .render(&scene, 200, 120, Theme::default())
            .expect("first");
        let second = renderer
            .render(&scene, 200, 120, Theme::default())
            .expect("second");
        assert_eq!(first.rgba, second.rgba);
    }

    #[test]
    fn a_polyline_writes_visible_pixels() {
        let scene = soil_scene::Scene::new(vec![soil_scene::Node {
            id: "line".into(),
            parent: None,
            transition_ms: None,
            transform: soil_scene::Transform {
                translation: [0.0, 0.0, 0.0],
                rotation: [0.0; 3],
                scale: [1.0, 1.0, 1.0],
            },
            content: soil_scene::Content::Polyline(soil_scene::Polyline {
                points: vec![[-3.0, -2.0], [3.0, 2.0]],
                ink: Some(soil_scene::Ink::Primary),
                rgb: None,
                rgb2: None,
                width: 0.1,
                opacity: 1.0,
                join: soil_scene::LineJoin::Miter,
                miter_limit: 4.0,
            }),
        }]);
        let frame = render(&scene, 200, 120, Theme::default()).expect("render");
        let lit = frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[2] > 100);
        assert!(lit, "polyline should leave visible pixels");
    }

    #[test]
    fn an_area_writes_visible_pixels() {
        let scene = soil_scene::Scene::new(vec![soil_scene::Node {
            id: "area".into(),
            parent: None,
            transition_ms: None,
            transform: soil_scene::Transform {
                translation: [0.0, 0.0, 0.0],
                rotation: [0.0; 3],
                scale: [1.0, 1.0, 1.0],
            },
            content: soil_scene::Content::Area(soil_scene::Area {
                points: vec![[-3.0, 2.0], [3.0, 2.0]],
                baseline: -2.0,
                ink: Some(soil_scene::Ink::Primary),
                rgb: None,
                opacity: 0.5,
                base_opacity: Some(0.0),
            }),
        }]);
        let frame = render(&scene, 200, 120, Theme::default()).expect("render");
        let lit = frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[2] > 100);
        assert!(lit, "area should leave visible pixels");
    }

    #[test]
    fn a_band_and_candles_write_visible_pixels() {
        let band = soil_scene::Content::Band(soil_scene::Band {
            upper: vec![[-3.0, 2.0], [3.0, 2.0]],
            lower: vec![[-3.0, 0.0], [3.0, 0.0]],
            ink: None,
            rgb: Some([255, 0, 0]),
            opacity: 1.0,
        });
        let candles = soil_scene::Content::Candles(soil_scene::Candles {
            candles: vec![soil_scene::Candle {
                x: 0.0,
                open: 1.0,
                high: 2.0,
                low: -1.0,
                close: 1.5,
            }],
            width: 0.2,
            up_ink: None,
            down_ink: None,
            up_rgb: Some([0, 255, 0]),
            down_rgb: Some([255, 0, 0]),
            opacity: 1.0,
        });
        let scene = soil_scene::Scene::new(vec![
            soil_scene::Node {
                id: "band".into(),
                parent: None,
                transition_ms: None,
                transform: soil_scene::Transform::default(),
                content: band,
            },
            soil_scene::Node {
                id: "candles".into(),
                parent: None,
                transition_ms: None,
                transform: soil_scene::Transform::default(),
                content: candles,
            },
        ]);
        let frame = render(&scene, 200, 120, Theme::default()).expect("render");
        let band_lit = frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[0] > 100 && pixel[1] < 80);
        let candle_lit = frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[1] > 100 && pixel[0] < 100);
        assert!(band_lit, "band should leave a red fill");
        assert!(candle_lit, "up candles should leave a green mark");
    }

    #[test]
    fn a_scene_produces_an_opaque_frame() {
        let scene = soil_scene::Scene::new(vec![soil_scene::Node {
            id: "ground".into(),
            parent: None,
            transition_ms: None,
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
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
    }
}
