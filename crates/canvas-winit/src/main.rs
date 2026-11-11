//! A native window for a GPUI Spore.
//!
//! This is the smallest canvas-level wrapper: winit owns the window, the Spore
//! owns the pixels. The host runs the component in a wasmtime store, receives
//! each `soil:present` frame, and blits it to the window. No scene, no layout,
//! no hit-testing here — those belong to the Spore.

use std::{
    collections::VecDeque,
    num::NonZeroU32,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use anyhow::{Context as _, Result};
use soil_spore::Manifest;
use soil_webgpu::{
    CanvasFrame, CanvasGpuPresenter, CatalogEntry, CatalogError, CatalogHost, CatalogLaunched,
    CatalogSurface, CatalogSurfaceKind, InputKey, WebGpuRuntime,
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key as WinitKey, NamedKey},
    window::{Window, WindowId},
};

type SoftContext = softbuffer::Context<Arc<Window>>;
type SoftSurface = softbuffer::Surface<Arc<Window>, Arc<Window>>;

const LOGICAL_SIZE: (f64, f64) = (1000.0, 700.0);

struct Installed {
    root: PathBuf,
    manifest: Manifest,
}

struct SporeCatalog {
    installed: Vec<Installed>,
}

impl SporeCatalog {
    fn new(root: &std::path::Path) -> Result<Self, CatalogError> {
        let mut installed = Vec::new();
        let Ok(directory) = std::fs::read_dir(root) else {
            return Ok(Self { installed });
        };
        for entry in directory.flatten() {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let package = entry.path();
            if !package.join("spore.toml").is_file() {
                continue;
            }
            let manifest = Manifest::load(&package).map_err(|_| CatalogError::Invalid)?;
            installed.push(Installed {
                root: package,
                manifest,
            });
        }
        installed.sort_by(|left, right| left.manifest.spore.name.cmp(&right.manifest.spore.name));
        Ok(Self { installed })
    }
}

impl CatalogHost for SporeCatalog {
    fn entries(&mut self) -> Result<Vec<CatalogEntry>, CatalogError> {
        Ok(self
            .installed
            .iter()
            .map(|installed| {
                let manifest = &installed.manifest;
                CatalogEntry {
                    id: manifest.spore.id.clone(),
                    name: manifest.spore.name.clone(),
                    version: manifest.spore.version.clone(),
                    summary: manifest.spore.summary.clone(),
                    workspace: None,
                    default_surface: manifest.launch.default.clone(),
                    surfaces: manifest
                        .surfaces
                        .iter()
                        .map(|surface| CatalogSurface {
                            name: surface.name.clone(),
                            kind: match surface.kind {
                                soil_spore::SurfaceKind::Canvas => CatalogSurfaceKind::Canvas,
                                soil_spore::SurfaceKind::Web => CatalogSurfaceKind::Web,
                            },
                            entry: surface
                                .entry(manifest.ui.as_ref())
                                .unwrap_or_default()
                                .to_owned(),
                            requires: surface.requires.clone(),
                        })
                        .collect(),
                }
            })
            .collect())
    }

    fn launch(&mut self, id: &str, surface: &str) -> Result<CatalogLaunched, CatalogError> {
        let installed = self
            .installed
            .iter()
            .find(|installed| installed.manifest.spore.id == id)
            .ok_or(CatalogError::NotFound)?;
        let module = installed
            .manifest
            .module
            .as_ref()
            .ok_or(CatalogError::Unsupported)?;
        let path = installed.root.join(&module.path);
        let mut child =
            std::process::Command::new(std::env::current_exe().map_err(|_| CatalogError::Io)?)
                .arg(&path)
                .spawn()
                .map_err(|_| CatalogError::Io)?;
        // Reap the child without blocking the launcher's event loop.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(CatalogLaunched {
            id: id.to_owned(),
            surface: surface.to_owned(),
            target: format!("canvas-winit {}", path.display()),
        })
    }
}

struct FramePresenter {
    size: (u32, u32),
    tx: mpsc::Sender<CanvasFrame>,
    window: Arc<Window>,
}

impl CanvasGpuPresenter for FramePresenter {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn present(&mut self, frame: CanvasFrame) -> Result<()> {
        eprintln!("canvas-winit: presenter {}x{}", frame.width, frame.height);
        self.tx
            .send(frame)
            .map_err(|_| anyhow::anyhow!("the window closed before the frame arrived"))?;
        // The channel does not wake winit's loop; a redraw request does.
        self.window.request_redraw();
        Ok(())
    }
}

struct App {
    module: PathBuf,
    tx: mpsc::Sender<CanvasFrame>,
    rx: mpsc::Receiver<CanvasFrame>,
    window: Option<Arc<Window>>,
    context: Option<SoftContext>,
    surface: Option<SoftSurface>,
    frame: Option<CanvasFrame>,
    capture: Option<PathBuf>,
    frames: usize,
    frames_seen: usize,
    input: Arc<Mutex<VecDeque<InputKey>>>,
    spores: PathBuf,
    started: bool,
}

impl App {
    fn start_component(&mut self, size: (u32, u32), window: Arc<Window>) {
        if self.started {
            return;
        }
        self.started = true;
        eprintln!("canvas-winit: starting component at {}x{}", size.0, size.1);
        let module = self.module.clone();
        let tx = self.tx.clone();
        let input = self.input.clone();
        let spores = self.spores.clone();
        thread::spawn(move || {
            let result = (|| -> Result<()> {
                let bytes =
                    std::fs::read(&module).with_context(|| format!("read {}", module.display()))?;
                let runtime = WebGpuRuntime::new()?;
                let catalog = SporeCatalog::new(&spores)
                    .map_err(|error| anyhow::anyhow!("catalog: {error:?}"))?;
                let status =
                    pollster::block_on(runtime.run_component_with_presenter_input_catalog(
                        &bytes,
                        Box::new(FramePresenter { size, tx, window }),
                        input,
                        true,
                        Box::new(catalog),
                    ))?;
                eprintln!("canvas-winit: guest returned {status}");
                anyhow::ensure!(status == 0, "guest returned status {status}");
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("canvas-winit: {error:#}");
            }
        });
    }

    fn take_frames(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(frame) = self.rx.try_recv() {
            eprintln!("canvas-winit: frame {}x{}", frame.width, frame.height);
            if let Some(path) = &self.capture
                && let Err(error) = std::fs::write(path, &frame.rgba)
            {
                eprintln!("canvas-winit: write {}: {error}", path.display());
            }
            self.frames_seen += 1;
            self.frame = Some(frame);
            if self.frames_seen >= self.frames {
                event_loop.exit();
            }
        }
    }

    fn paint(&mut self) {
        let (Some(surface), Some(frame)) = (self.surface.as_mut(), self.frame.as_ref()) else {
            return;
        };
        let (Some(width), Some(height)) =
            (NonZeroU32::new(frame.width), NonZeroU32::new(frame.height))
        else {
            return;
        };
        if let Err(error) = surface.resize(width, height) {
            eprintln!("canvas-winit: resize: {error}");
            return;
        }
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(error) => {
                eprintln!("canvas-winit: buffer: {error}");
                return;
            }
        };
        for (pixel, rgba) in buffer.iter_mut().zip(frame.rgba.chunks_exact(4)) {
            *pixel = (u32::from(rgba[0]) << 16) | (u32::from(rgba[1]) << 8) | u32::from(rgba[2]);
        }
        if let Err(error) = buffer.present() {
            eprintln!("canvas-winit: present: {error}");
        } else {
            eprintln!("canvas-winit: painted {}x{}", frame.width, frame.height);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(format!(
                "canvas-winit — {}",
                self.module
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("spore")
            ))
            .with_inner_size(LogicalSize::new(LOGICAL_SIZE.0, LOGICAL_SIZE.1))
            .with_resizable(false);
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("canvas-winit: window: {error}");
                event_loop.exit();
                return;
            }
        };
        let context = match SoftContext::new(window.clone()) {
            Ok(context) => context,
            Err(error) => {
                eprintln!("canvas-winit: softbuffer context: {error}");
                event_loop.exit();
                return;
            }
        };
        let surface = match SoftSurface::new(&context, window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                eprintln!("canvas-winit: softbuffer surface: {error}");
                event_loop.exit();
                return;
            }
        };

        let scale = window.scale_factor();
        let physical = LogicalSize::new(LOGICAL_SIZE.0, LOGICAL_SIZE.1).to_physical::<u32>(scale);
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
        self.start_component(
            (physical.width, physical.height),
            self.window.as_ref().unwrap().clone(),
        );
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                self.take_frames(event_loop);
                self.paint();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    let key = key_name(&event.logical_key);
                    let text = event.text.as_ref().map(|text| text.to_string());
                    self.input
                        .lock()
                        .expect("input queue poisoned")
                        .push_back(InputKey {
                            key,
                            text,
                            pressed: true,
                            shift: false,
                            control: false,
                            alt: false,
                            meta: false,
                        });
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let before = self.frames_seen;
        self.take_frames(event_loop);
        if self.frames_seen > before
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }
}

fn key_name(key: &WinitKey) -> String {
    match key {
        WinitKey::Named(NamedKey::Enter) => "enter".into(),
        WinitKey::Named(NamedKey::Escape) => "escape".into(),
        WinitKey::Named(NamedKey::ArrowDown) => "arrow-down".into(),
        WinitKey::Named(NamedKey::ArrowUp) => "arrow-up".into(),
        WinitKey::Named(NamedKey::ArrowLeft) => "arrow-left".into(),
        WinitKey::Named(NamedKey::ArrowRight) => "arrow-right".into(),
        WinitKey::Named(NamedKey::Space) => "space".into(),
        WinitKey::Named(NamedKey::Tab) => "tab".into(),
        WinitKey::Named(NamedKey::Backspace) => "backspace".into(),
        WinitKey::Character(text) => text.to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn main() -> Result<()> {
    let mut module = None;
    let mut capture = None;
    let mut frames = None;
    let mut key = None;
    let mut spores = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--capture" => {
                capture = Some(PathBuf::from(
                    args.next().context("--capture needs a path")?,
                ));
            }
            "--frames" => {
                frames = Some(
                    args.next()
                        .context("--frames needs a count")?
                        .parse()
                        .context("--frames needs a number")?,
                );
            }
            "--key" => key = Some(args.next().context("--key needs a key name")?),
            "--spores" => {
                spores = Some(PathBuf::from(
                    args.next().context("--spores needs a directory")?,
                ));
            }
            value if module.is_none() => module = Some(PathBuf::from(value)),
            _ => anyhow::bail!(
                "usage: canvas-winit <module.wasm> [--capture FILE.rgba] [--frames N] [--key KEY] [--spores DIR]"
            ),
        }
    }
    let module = module.context(
        "usage: canvas-winit <module.wasm> [--capture FILE.rgba] [--frames N] [--key KEY] [--spores DIR]",
    )?;
    let input = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(key) = key {
        let text = (key.chars().count() == 1).then(|| key.clone());
        input
            .lock()
            .expect("input queue poisoned")
            .push_back(InputKey {
                key,
                text,
                pressed: true,
                shift: false,
                control: false,
                alt: false,
                meta: false,
            });
    }
    let (tx, rx) = mpsc::channel();
    let mut app = App {
        module,
        tx,
        rx,
        window: None,
        context: None,
        surface: None,
        frame: None,
        capture,
        frames: frames.unwrap_or(1),
        frames_seen: 0,
        input,
        spores: spores.unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join("Library/Application Support/fungOS/spores")
        }),
        started: false,
    };
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    Ok(())
}
