// A real event source: winit drives the same router the headless path uses.
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::input::{Button, Intent, Mods, Region, Router};
use crate::Ui;
use vello::kurbo::Affine;

pub struct Host {
    ui: Ui,
    router: Router,
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    config: Option<wgpu::SurfaceConfiguration>,
    renderer: Option<vello::Renderer>,
    /// The host owns these: a view registers actions, it does not mutate state.
    selected: String,
    mods: Mods,
    scroll: f32,
    zoom_bias: f64,
}

impl Host {
    pub fn new(ui: Ui) -> Self {
        Self {
            ui,
            router: Router::new(),
            window: None,
            surface: None,
            device: None,
            queue: None,
            config: None,
            renderer: None,
            selected: "studio".to_string(),
            scroll: 0.0,
            mods: Mods::default(),
            zoom_bias: 1.0,
        }
    }

    fn regions(&self) -> Vec<Region> {
        self.ui.regions()
    }

    /// Route one activation: the action string is the view's request, the host
    /// decides what it means. `open:<name>` is what a nav row asks for.
    fn activate(&mut self, id: &str) {
        let Some(action) = self.ui.action(id) else { return };
        eprintln!("action {id} -> {action:?}");
        if let Some(name) = action.strip_prefix("open:") {
            self.selected = name.to_string();
        }
    }

    fn redraw(&mut self) {
        let (Some(_window), Some(surface), Some(device), Some(queue), Some(config), Some(renderer)) = (
            self.window.as_ref(), self.surface.as_ref(), self.device.as_ref(),
            self.queue.as_ref(), self.config.as_ref(), self.renderer.as_mut(),
        ) else { return };

        let state = crate::UiState {
            hover: self.router.hover().unwrap_or("").to_string(),
            pressed: self.router.pressed().unwrap_or("").to_string(),
            focus: self.router.focus().unwrap_or("").to_string(),
            selected: self.selected.clone(),
            scroll: self.scroll,
            pointer: self.router.cursor_position(),
            mods: self.mods,
            drag: None,
        };
        let zoom = {
            let base = (config.width as f64 / 1000.0).clamp(1.0, 2.0);
            (base * self.zoom_bias).clamp(0.5, 3.0)
        };
        let logical_width = (config.width as f64 / zoom).round().max(1.0) as u32;
        let logical_height = (config.height as f64 / zoom).round().max(1.0) as u32;
        if let Err(error) = self.ui.paint(logical_width, logical_height, &state) {
            eprintln!("paint: {error}");
            return;
        }
        // The view lays out at logical size; the surface renders it at zoom.
        // Text stays vector here, so a bigger window gets bigger type, not blur.
        let mut scene = vello::Scene::new();
        scene.append(&self.ui.scene(), Some(Affine::scale(zoom)));

        let frame = match surface.get_current_texture() {
            Ok(frame) => frame,
            Err(error) => { eprintln!("surface: {error}"); return; }
        };
        let params = vello::RenderParams {
            base_color: vello::peniko::Color::from_rgb8(11, 14, 17),
            width: config.width,
            height: config.height,
            antialiasing_method: vello::AaConfig::Area,
        };
        if let Err(error) = renderer.render_to_surface(device, queue, &scene, &frame, &params) {
            eprintln!("render: {error}");
            return;
        }
        frame.present();
    }
}

impl ApplicationHandler for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() { return; }
        let window = Arc::new(event_loop.create_window(Window::default_attributes().with_title("soil · spore demo")).unwrap());
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        })).expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).expect("device");
        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        // vello renders into non-srgb targets; pick one the surface offers.
        let format = caps.formats.iter().copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm))
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let renderer = vello::Renderer::new(&device, vello::RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: std::num::NonZeroUsize::new(1),
            surface_format: Some(format),
        }).expect("renderer");

        self.surface = Some(surface);
        self.device = Some(device);
        self.queue = Some(queue);
        self.config = Some(config);
        self.renderer = Some(renderer);
        self.window = Some(window);
        self.redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let regions = self.regions();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(size) => {
                if let (Some(surface), Some(device), Some(config)) = (&self.surface, &self.device, self.config.as_mut()) {
                    config.width = size.width.max(1);
                    config.height = size.height.max(1);
                    surface.configure(device, config);
                }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let state = modifiers.state();
                self.mods = Mods {
                    cmd: state.super_key(),
                    shift: state.shift_key(),
                    alt: state.alt_key(),
                    ctrl: state.control_key(),
                };
            }
            WindowEvent::CursorMoved { position, .. } => {
                let intents = self.router.pointer_moved(&regions, position.x as f32, position.y as f32);
                for intent in intents {
                    eprintln!("{intent:?}");
                }
                self.redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.router.pointer_left();
                self.redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let at = self.router.cursor_position();
                let button = match button {
                    MouseButton::Left => Button::Primary,
                    MouseButton::Right => Button::Secondary,
                    _ => Button::Middle,
                };
                let intents = match state {
                    ElementState::Pressed => self.router.pointer_down(&regions, button, at.0, at.1, self.mods),
                    ElementState::Released => self.router.pointer_up(&regions, at.0, at.1, self.mods),
                };
                for intent in intents {
                    eprintln!("intent {intent:?}");
                    if let Intent::Activate { id, .. } = intent {
                        self.activate(&id);
                    }
                }
                self.redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let at = self.router.cursor_position();
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 20.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.scroll = (self.scroll - dy).max(0.0);
                for intent in self.router.wheel(&regions, at.0, at.1, dy, self.mods) {
                    eprintln!("intent {intent:?}");
                }
                self.redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                match event.logical_key {
                    // Tab walks the hit regions in paint order; the view draws
                    // the ring, so the host only moves focus.
                    Key::Named(NamedKey::Tab) => {
                        self.router.focus_next(&regions, self.mods.shift);
                        self.redraw();
                    }
                    // Enter or Space activates whatever has focus, through the
                    // same action path a click takes.
                    Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                        if let Some(id) = self.router.focus().map(str::to_string) {
                            self.activate(&id);
                            self.redraw();
                        }
                    }
                    Key::Named(NamedKey::Escape) => {
                        self.router.set_focus(None);
                        self.redraw();
                    }
                    // Cmd/Ctrl with +/- scales the whole surface, type included.
                    Key::Character(ref character) if self.mods.cmd || self.mods.ctrl => {
                        match character.as_str() {
                            "+" | "=" => self.zoom_bias = (self.zoom_bias * 1.15).min(2.5),
                            "-" => self.zoom_bias = (self.zoom_bias / 1.15).max(0.5),
                            _ => return,
                        }
                        self.redraw();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

pub fn run(ui: Ui) -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    let mut host = Host::new(ui);
    event_loop.run_app(&mut host)?;
    Ok(())
}
