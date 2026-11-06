//! A native Soil window for a GPU visualization scene.
//!
//! The scene is rendered by the same `soil_scene_gpu` path Canvas leases use,
//! then blitted to a winit/wgpu surface. This is the "Soil owns the host, not
//! just the compositor" demo: chart, graph, map and atlas layers in one native
//! window, with no Spore or Canvas process involved.

use std::sync::Arc;
use std::time::SystemTime;

use soil_scene_gpu::{Frame, Theme};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const BLIT_SHADER: &str = r#"
struct VOut {
  @builtin(position) position: vec4<f32>,
  @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VOut {
  var pos = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 3.0, -1.0),
    vec2<f32>(-1.0,  3.0),
  );
  var uv = array<vec2<f32>, 3>(
    vec2<f32>(0.0, 1.0),
    vec2<f32>(2.0, 1.0),
    vec2<f32>(0.0, -1.0),
  );
  return VOut(vec4<f32>(pos[index], 0.0, 1.0), uv[index]);
}
@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;
@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
  return textureSample(frame_tex, frame_sampler, in.uv);
}
"#;

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: Option<wgpu::Texture>,
    bind_group: Option<wgpu::BindGroup>,
}

impl Gpu {
    fn new(window: Arc<Window>, size: PhysicalSize<u32>) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("soil-viz"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: Default::default(),
            }))?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: Vec::new(),
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("soil-viz blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soil-viz frame layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("soil-viz pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("soil-viz blit pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("soil-viz sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            layout,
            sampler,
            texture: None,
            bind_group: None,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }

    fn upload(&mut self, frame: &Frame) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("soil-viz frame"),
            size: wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("soil-viz frame bind group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        }));
        self.texture = Some(texture);
    }

    fn draw(&mut self) -> anyhow::Result<()> {
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("soil-viz present"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("soil-viz blit pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(bind_group) = &self.bind_group {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        output.present();
        Ok(())
    }
}

struct App {
    scene_path: String,
    watch: bool,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    scene: soil_scene::Scene,
    last_modified: Option<SystemTime>,
    dirty: bool,
}

impl App {
    fn reload(&mut self) -> anyhow::Result<()> {
        let bytes = std::fs::read(&self.scene_path)?;
        let present: soil_scene::Present = serde_json::from_slice(&bytes)?;
        let soil_scene::Present::Scene(scene) = present else {
            anyhow::bail!("scene is not a soil_scene Scene");
        };
        scene.validate().map_err(|error| anyhow::anyhow!(error))?;
        self.scene = scene;
        self.last_modified = modified(&self.scene_path);
        self.dirty = true;
        Ok(())
    }

    fn redraw(&mut self) {
        let (Some(window), Some(gpu)) = (&self.window, &mut self.gpu) else {
            return;
        };
        if self.dirty {
            let size = window.inner_size();
            match soil_scene_gpu::render(
                &self.scene,
                size.width.max(1),
                size.height.max(1),
                Theme::default(),
            ) {
                Ok(frame) => gpu.upload(&frame),
                Err(error) => eprintln!("soil-viz: render failed: {error:#}"),
            }
            self.dirty = false;
        }
        if let Err(error) = gpu.draw() {
            eprintln!("soil-viz: present failed: {error:#}");
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = winit::window::Window::default_attributes()
            .with_title("Soil · visualization layers")
            .with_inner_size(winit::dpi::LogicalSize::new(1200.0, 840.0));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create soil-viz window"),
        );
        let size = window.inner_size();
        match Gpu::new(window.clone(), size) {
            Ok(gpu) => self.gpu = Some(gpu),
            Err(error) => {
                eprintln!("soil-viz: GPU init failed: {error:#}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window.clone());
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size);
                }
                self.dirty = true;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if matches!(
                    event.logical_key,
                    Key::Named(NamedKey::Escape) | Key::Character(_)
                ) {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.watch {
            if modified(&self.scene_path) != self.last_modified {
                match self.reload() {
                    Ok(()) => {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    Err(error) => eprintln!("soil-viz: reload failed: {error:#}"),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    }
}

fn modified(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut scene_path = None;
    let mut watch = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scene" => scene_path = args.next(),
            "--watch" => watch = true,
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let scene_path = scene_path.ok_or_else(|| anyhow::anyhow!("missing --scene"))?;
    let bytes = std::fs::read(&scene_path)?;
    let present: soil_scene::Present = serde_json::from_slice(&bytes)?;
    let soil_scene::Present::Scene(scene) = present else {
        anyhow::bail!("scene is not a soil_scene Scene");
    };
    scene.validate().map_err(|error| anyhow::anyhow!(error))?;
    let last_modified = modified(&scene_path);
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        scene_path,
        watch,
        window: None,
        gpu: None,
        scene,
        last_modified,
        dirty: true,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
