//! A native Soil terminal window.
//!
//! A real pty is attached to `soil_vt::Host`; the emulator publishes grids on
//! its own thread, and this window turns the latest grid into the same GPU
//! frame Canvas leases. There is no Canvas process and no browser in the path.
//!
//! ```text
//! keyboard ──► pty ──► soil-vt Host ──► GPU terminal scene ──► winit surface
//! ```

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use soil_scene_gpu::{Frame, Theme};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
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
                label: Some("soil-vt-window"),
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
            label: Some("soil-vt-window blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("soil-vt-window frame layout"),
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
            label: Some("soil-vt-window pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("soil-vt-window blit pipeline"),
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
            label: Some("soil-vt-window sampler"),
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
            label: Some("soil-vt-window frame"),
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
            label: Some("soil-vt-window frame bind group"),
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
                label: Some("soil-vt-window present"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("soil-vt-window blit pass"),
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

struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pty {
    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    fn exited(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_some()
    }
}

struct App {
    command: Vec<String>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    renderer: Option<soil_scene_gpu::Renderer>,
    pty: Option<Pty>,
    host: Option<soil_vt::Host>,
    cols: u16,
    rows: u16,
    last_generation: u64,
    last_title: String,
    last_draw: Instant,
    modifiers: ModifiersState,
    dirty: bool,
}

impl App {
    fn new(command: Vec<String>) -> Self {
        Self {
            command,
            window: None,
            gpu: None,
            renderer: None,
            pty: None,
            host: None,
            cols: 100,
            rows: 32,
            last_generation: u64::MAX,
            last_title: String::new(),
            last_draw: Instant::now(),
            modifiers: ModifiersState::empty(),
            dirty: true,
        }
    }

    fn start_pty(&mut self) -> anyhow::Result<()> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut builder = CommandBuilder::new(&self.command[0]);
        builder.args(&self.command[1..]);
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        builder.env("TERM_PROGRAM", "soil-vt-window");
        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
        let host = soil_vt::Host::spawn(self.cols, self.rows, 10_000)?;
        let reader_host = host.clone();
        std::thread::Builder::new()
            .name("soil-vt-pty-reader".into())
            .spawn(move || {
                let mut buffer = [0u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => reader_host.write(&buffer[..n]),
                    }
                }
            })?;
        self.pty = Some(Pty {
            master: pair.master,
            writer,
            child,
        });
        self.host = Some(host);
        Ok(())
    }

    fn set_grid_for_window(&mut self, size: PhysicalSize<u32>) {
        // Keep the same cell aspect as the GPU terminal renderer: roughly
        // 12x22 logical pixels per cell, with a little room for margins.
        let cols = ((size.width as f32 - 20.0) / 12.0).floor().max(20.0) as u16;
        let rows = ((size.height as f32 - 20.0) / 22.0).floor().max(5.0) as u16;
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        if let Some(host) = &self.host {
            host.resize(cols, rows);
        }
        if let Some(pty) = &self.pty {
            pty.resize(cols, rows);
        }
        self.dirty = true;
    }

    fn refresh(&mut self) {
        let Some(host) = &self.host else {
            return;
        };
        let generation = host.generation();
        if generation != self.last_generation {
            let snapshot = host.snapshot();
            if !snapshot.title.is_empty() && snapshot.title != self.last_title {
                self.last_title = snapshot.title.clone();
                if let Some(window) = &self.window {
                    window.set_title(&snapshot.title);
                }
            }
            if let (Some(window), Some(gpu), Some(renderer)) =
                (&self.window, &mut self.gpu, &self.renderer)
            {
                let size = window.inner_size();
                if size.width > 0 && size.height > 0 {
                    let scene = soil_scene_gpu::terminal::terminal_scene(
                        &snapshot,
                        size.width,
                        size.height,
                        Theme::default(),
                    );
                    match renderer.render(&scene, size.width, size.height, Theme::default()) {
                        Ok(frame) => gpu.upload(&frame),
                        Err(error) => eprintln!("soil-vt-window: render failed: {error:#}"),
                    }
                }
            }
            self.last_generation = generation;
            self.last_draw = Instant::now();
            self.dirty = true;
        }
        let responses = host.drain_responses();
        if !responses.is_empty()
            && let Some(pty) = &self.pty
        {
            pty.write(&responses);
        }
    }

    fn redraw(&mut self) {
        if self.dirty {
            self.dirty = false;
            if let Some(gpu) = &mut self.gpu
                && let Err(error) = gpu.draw()
            {
                eprintln!("soil-vt-window: present failed: {error:#}");
            }
        }
    }

    fn send_key(&self, event: &winit::event::KeyEvent) {
        let Some(pty) = &self.pty else {
            return;
        };
        let bytes = key_bytes(&event.logical_key, event.text.as_deref(), self.modifiers);
        if let Some(bytes) = bytes
            && !bytes.is_empty()
        {
            pty.write(&bytes);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = winit::window::Window::default_attributes()
            .with_title("Soil · terminal")
            .with_inner_size(LogicalSize::new(1200.0, 760.0));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create soil-vt-window window"),
        );
        let size = window.inner_size();
        match Gpu::new(window.clone(), size) {
            Ok(gpu) => self.gpu = Some(gpu),
            Err(error) => {
                eprintln!("soil-vt-window: GPU init failed: {error:#}");
                event_loop.exit();
                return;
            }
        }
        match soil_scene_gpu::Renderer::new() {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                eprintln!("soil-vt-window: renderer init failed: {error:#}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window.clone());
        self.set_grid_for_window(size);
        if let Err(error) = self.start_pty() {
            eprintln!("soil-vt-window: pty failed: {error:#}");
            event_loop.exit();
            return;
        }
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
                self.set_grid_for_window(size);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    // Ctrl+Shift+Q is a host escape hatch. Everything else,
                    // including Escape and Ctrl+C, belongs to the shell.
                    if self.modifiers.control_key()
                        && self.modifiers.shift_key()
                        && matches!(event.logical_key, Key::Character(ref text) if text.eq_ignore_ascii_case("q"))
                    {
                        event_loop.exit();
                        return;
                    }
                    self.send_key(&event);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(pty) = &mut self.pty
            && pty.exited()
        {
            event_loop.exit();
            return;
        }
        self.refresh();
        if self.dirty
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
        let delay = if self.last_draw.elapsed() < Duration::from_millis(8) {
            Duration::from_millis(8)
        } else {
            Duration::from_millis(16)
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + delay));
    }
}

fn key_bytes(key: &Key, text: Option<&str>, modifiers: ModifiersState) -> Option<Vec<u8>> {
    // Host/OS shortcuts stay with the OS. Sending Command+C as literal text
    // would make copy/paste impossible and surprise every shell user.
    if modifiers.super_key() {
        return None;
    }
    let alt = modifiers.alt_key();
    let ctrl = modifiers.control_key();

    if let Key::Character(value) = key {
        if ctrl {
            let ch = value.chars().next()?;
            let upper = ch.to_ascii_uppercase();
            if upper.is_ascii() {
                let byte = upper as u8;
                if (b'@'..=b'_').contains(&byte) {
                    return Some(vec![byte & 0x1f]);
                }
            }
            return None;
        }
        let mut bytes = text.unwrap_or(value.as_str()).as_bytes().to_vec();
        if alt {
            bytes.insert(0, 0x1b);
        }
        return Some(bytes);
    }

    let sequence: &[u8] = match key {
        Key::Named(NamedKey::Enter) => b"\r",
        Key::Named(NamedKey::Backspace) => b"\x7f",
        Key::Named(NamedKey::Tab) => b"\t",
        Key::Named(NamedKey::Escape) => b"\x1b",
        Key::Named(NamedKey::ArrowUp) => b"\x1b[A",
        Key::Named(NamedKey::ArrowDown) => b"\x1b[B",
        Key::Named(NamedKey::ArrowRight) => b"\x1b[C",
        Key::Named(NamedKey::ArrowLeft) => b"\x1b[D",
        Key::Named(NamedKey::Home) => b"\x1b[H",
        Key::Named(NamedKey::End) => b"\x1b[F",
        Key::Named(NamedKey::Insert) => b"\x1b[2~",
        Key::Named(NamedKey::Delete) => b"\x1b[3~",
        Key::Named(NamedKey::PageUp) => b"\x1b[5~",
        Key::Named(NamedKey::PageDown) => b"\x1b[6~",
        Key::Named(NamedKey::F1) => b"\x1bOP",
        Key::Named(NamedKey::F2) => b"\x1bOQ",
        Key::Named(NamedKey::F3) => b"\x1bOR",
        Key::Named(NamedKey::F4) => b"\x1bOS",
        Key::Named(NamedKey::F5) => b"\x1b[15~",
        Key::Named(NamedKey::F6) => b"\x1b[17~",
        Key::Named(NamedKey::F7) => b"\x1b[18~",
        Key::Named(NamedKey::F8) => b"\x1b[19~",
        Key::Named(NamedKey::F9) => b"\x1b[20~",
        Key::Named(NamedKey::F10) => b"\x1b[21~",
        Key::Named(NamedKey::F11) => b"\x1b[23~",
        Key::Named(NamedKey::F12) => b"\x1b[24~",
        _ => return None,
    };
    let mut bytes = sequence.to_vec();
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn main() -> anyhow::Result<()> {
    let mut command = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shell" => {
                command = vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())];
            }
            "--command" => {
                command = args.by_ref().collect();
                break;
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    if command.is_empty() {
        command = vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())];
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(command);
    event_loop.run_app(&mut app)?;
    Ok(())
}
