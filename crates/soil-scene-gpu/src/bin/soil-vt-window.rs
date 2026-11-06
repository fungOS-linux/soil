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
use soil_input::{Key as InputKey, Modifiers, terminal_bytes};
use soil_scene_gpu::Theme;
use soil_scene_gpu::window::WindowRenderer;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

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
    renderer: Option<WindowRenderer>,
    scene: soil_scene::Scene,
    pty: Option<Pty>,
    host: Option<soil_vt::Host>,
    cols: u16,
    rows: u16,
    last_generation: u64,
    last_title: String,
    last_draw: Instant,
    modifiers: Modifiers,
    dirty: bool,
}

impl App {
    fn new(command: Vec<String>) -> Self {
        Self {
            command,
            window: None,
            renderer: None,
            scene: soil_scene::Scene::new(Vec::new()),
            pty: None,
            host: None,
            cols: 100,
            rows: 32,
            last_generation: u64::MAX,
            last_title: String::new(),
            last_draw: Instant::now(),
            modifiers: Modifiers::default(),
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
            if let Some(window) = &self.window {
                let size = window.inner_size();
                if size.width > 0 && size.height > 0 {
                    self.scene = soil_scene_gpu::terminal::terminal_scene(
                        &snapshot,
                        size.width,
                        size.height,
                        Theme::terminal(),
                    );
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
            if let Some(renderer) = &mut self.renderer
                && let Err(error) = renderer.present(&self.scene, Theme::terminal())
            {
                eprintln!("soil-vt-window: present failed: {error:#}");
            }
        }
    }

    fn send_key(&self, event: &winit::event::KeyEvent) {
        let Some(pty) = &self.pty else {
            return;
        };
        let Some(key) = InputKey::from_winit(&event.logical_key) else {
            return;
        };
        let bytes = terminal_bytes(&key, event.text.as_deref(), self.modifiers);
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
        match WindowRenderer::new(window.clone(), size) {
            Ok(mut renderer) => {
                renderer.set_font(soil_scene_gpu::load_mono_font());
                self.renderer = Some(renderer);
            }
            Err(error) => {
                eprintln!("soil-vt-window: GPU init failed: {error:#}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window.clone());
        window.focus_window();
        window.set_ime_allowed(true);
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
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size);
                }
                self.set_grid_for_window(size);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state().into();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                if let Some(window) = &self.window {
                    window.focus_window();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    // Ctrl+Shift+Q is a host escape hatch. Everything else,
                    // including Escape and Ctrl+C, belongs to the shell.
                    if self.modifiers.control
                        && self.modifiers.shift
                        && matches!(
                            InputKey::from_winit(&event.logical_key),
                            Some(InputKey::Character(ref text)) if text.eq_ignore_ascii_case("q")
                        )
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
