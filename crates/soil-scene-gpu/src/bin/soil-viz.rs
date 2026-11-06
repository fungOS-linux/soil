//! Native Soil visualization window.
//!
//! `--scene` plays a captured `soil_scene`. `--live` runs the resident Rust
//! live host from a Lua-authored `LiveSpec`. Both render directly into the
//! swapchain with one device: no offscreen readback and no texture upload.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use soil_input::{
    InputEvent, Key, KeyEvent, Modifiers, NamedKey, Point, PointerButton, PointerEvent,
    PointerPhase,
};
use soil_scene_gpu::Theme;
use soil_scene_gpu::live::LiveViz;
use soil_scene_gpu::window::WindowRenderer;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct App {
    scene_path: String,
    watch: bool,
    live: Option<LiveViz>,
    live_path: Option<String>,
    last_modified: Option<SystemTime>,
    last_tick: Instant,
    fps: u32,
    scene: soil_scene::Scene,
    window: Option<Arc<Window>>,
    renderer: Option<WindowRenderer>,
    dirty: bool,
    paused: bool,
    pointer: Point,
    modifiers: Modifiers,
    quit: bool,
}

impl App {
    fn reload_scene(&mut self) -> anyhow::Result<()> {
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

    fn reload_live(&mut self) -> anyhow::Result<()> {
        let Some(path) = self.live_path.as_deref() else {
            return Ok(());
        };
        let bytes = std::fs::read(path)?;
        let spec: soil_scene_gpu::live::LiveSpec = serde_json::from_slice(&bytes)?;
        let live = LiveViz::new(spec);
        self.scene = live.scene();
        self.live = Some(live);
        self.last_modified = modified(path);
        self.last_tick = Instant::now();
        self.dirty = true;
        Ok(())
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Pointer(pointer) => {
                if let Some(window) = &self.window {
                    window.focus_window();
                }
                if pointer.phase != PointerPhase::Press {
                    return;
                }
                let spec_size = self
                    .live
                    .as_ref()
                    .map(|live| (live.spec().width, live.spec().height));
                let (px, py) = if let (Some(window), Some((sw, sh))) = (&self.window, spec_size) {
                    let size = window.inner_size();
                    (
                        pointer.x * sw / size.width.max(1) as f32,
                        pointer.y * sh / size.height.max(1) as f32,
                    )
                } else {
                    (pointer.x, pointer.y)
                };
                if let Some(live) = &mut self.live
                    && let Some(soil_scene_gpu::live::HitAction::ToggleSeries { layer, series }) =
                        live.hit_test(px, py)
                {
                    live.toggle_series(layer, series);
                    self.scene = live.scene();
                    self.dirty = true;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            InputEvent::Key(key) => {
                if key.repeat {
                    return;
                }
                match &key.key {
                    Key::Named(NamedKey::Space) => {
                        self.paused = !self.paused;
                        self.last_tick = Instant::now();
                        if let Some(window) = &self.window {
                            window.set_title(if self.paused {
                                "Soil · visualization layers (paused)"
                            } else {
                                "Soil · visualization layers"
                            });
                        }
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("r") => {
                        if let Some(live) = &mut self.live {
                            live.reset();
                            self.scene = live.scene();
                            self.dirty = true;
                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                        }
                    }
                    Key::Character(text) if text.eq_ignore_ascii_case("q") => self.quit = true,
                    Key::Named(NamedKey::Escape) => self.quit = true,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn redraw(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if let Some(renderer) = &mut self.renderer
            && let Err(error) = renderer.present(&self.scene, Theme::default())
        {
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
        match WindowRenderer::new(window.clone(), size) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                eprintln!("soil-viz: GPU init failed: {error:#}");
                event_loop.exit();
                return;
            }
        }
        if let Some(live) = &mut self.live {
            live.set_scale(window.scale_factor() as f32);
            self.scene = live.scene();
        }
        self.window = Some(window.clone());
        self.last_tick = Instant::now();
        window.focus_window();
        window.set_ime_allowed(true);
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
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size);
                }
                self.dirty = true;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(live) = &mut self.live {
                    live.set_scale(scale_factor as f32);
                    self.scene = live.scene();
                    self.dirty = true;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state().into();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = if let Some(window) = &self.window {
                    let size = window.inner_size();
                    (
                        position.x as f32 / size.width.max(1) as f32,
                        position.y as f32 / size.height.max(1) as f32,
                    )
                } else {
                    (position.x as f32, position.y as f32)
                };
                self.pointer = Point { x, y }.clamp_unit();
                if let Some(live) = &mut self.live {
                    let spec = (live.spec().width, live.spec().height);
                    live.set_pointer(Some((self.pointer.x * spec.0, self.pointer.y * spec.1)));
                    self.scene = live.scene();
                    self.dirty = true;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(live) = &mut self.live {
                    live.set_pointer(None);
                    self.scene = live.scene();
                    self.dirty = true;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let phase = match state {
                    ElementState::Pressed => PointerPhase::Press,
                    ElementState::Released => PointerPhase::Release,
                };
                let button = match button {
                    MouseButton::Left => Some(PointerButton::Primary),
                    MouseButton::Right => Some(PointerButton::Secondary),
                    MouseButton::Middle => Some(PointerButton::Middle),
                    _ => None,
                };
                self.handle_input(InputEvent::Pointer(PointerEvent {
                    phase,
                    x: self.pointer.x,
                    y: self.pointer.y,
                    pointer_id: 1,
                    button,
                    modifiers: self.modifiers,
                    at_ms: Some(unix_ms()),
                }));
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(key) = Key::from_winit(&event.logical_key) {
                    self.handle_input(InputEvent::Key(KeyEvent {
                        key,
                        text: event.text.as_deref().map(str::to_owned),
                        repeat: event.repeat,
                        modifiers: self.modifiers,
                        at_ms: Some(unix_ms()),
                    }));
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.quit {
            event_loop.exit();
            return;
        }
        let interval = Duration::from_millis(1000 / u64::from(self.fps.max(1)));
        if self.live.is_some() {
            if let Some(path) = self.live_path.as_deref()
                && modified(path) != self.last_modified
            {
                match self.reload_live() {
                    Ok(()) => {}
                    Err(error) => eprintln!("soil-viz: live spec reload failed: {error:#}"),
                }
            }
            let now = Instant::now();
            if !self.paused && now.duration_since(self.last_tick) >= interval {
                let scene = self.live.as_mut().and_then(|live| live.tick(now));
                if let Some(scene) = scene {
                    self.scene = scene;
                    self.dirty = true;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                self.last_tick = now;
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.last_tick + interval));
            return;
        }
        if self.watch {
            if modified(&self.scene_path) != self.last_modified {
                match self.reload_scene() {
                    Ok(()) => {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    Err(error) => eprintln!("soil-viz: reload failed: {error:#}"),
                }
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + interval));
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

fn modified(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut scene_path = None;
    let mut live_path = None;
    let mut watch = false;
    let mut fps = 60u32;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scene" => scene_path = args.next(),
            "--live" => live_path = args.next(),
            "--watch" => watch = true,
            "--fps" => {
                fps = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--fps needs a number"))?
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid --fps"))?;
                fps = fps.clamp(1, 240);
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let (scene, live, live_path, scene_path) = if let Some(live_path) = live_path {
        let bytes = std::fs::read(&live_path)?;
        let spec: soil_scene_gpu::live::LiveSpec = serde_json::from_slice(&bytes)?;
        if let Some(spec_fps) = spec.fps {
            fps = fps.min(spec_fps.max(1));
        }
        let live = LiveViz::new(spec);
        let scene = live.scene();
        (scene, Some(live), Some(live_path.clone()), live_path)
    } else {
        let scene_path = scene_path.ok_or_else(|| anyhow::anyhow!("missing --scene or --live"))?;
        let bytes = std::fs::read(&scene_path)?;
        let present: soil_scene::Present = serde_json::from_slice(&bytes)?;
        let soil_scene::Present::Scene(scene) = present else {
            anyhow::bail!("scene is not a soil_scene Scene");
        };
        scene.validate().map_err(|error| anyhow::anyhow!(error))?;
        (scene, None, None, scene_path)
    };
    let last_modified = modified(live_path.as_deref().unwrap_or(&scene_path));
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        scene_path,
        watch,
        live,
        live_path,
        last_modified,
        last_tick: Instant::now(),
        fps,
        scene,
        window: None,
        renderer: None,
        dirty: true,
        paused: false,
        pointer: Point::default(),
        modifiers: Modifiers::default(),
        quit: false,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
