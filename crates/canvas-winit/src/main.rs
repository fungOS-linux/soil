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

mod terminal;

use soil_webgpu::{
    CanvasFrame, CanvasGpuPresenter, CatalogEntry, CatalogError, CatalogHost, CatalogLaunched,
    CatalogSurface, CatalogSurfaceKind, CursorHost, CursorStyle, HostSetup, InputEvent, InputKey,
    PointerButton, WebGpuRuntime,
};
use terminal::PtyTerminal;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key as WinitKey, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
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
                    title: manifest
                        .spore
                        .title
                        .clone()
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or_else(|| title_case(&manifest.spore.name)),
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
        let executable = std::env::current_exe().map_err(|_| CatalogError::Io)?;
        let mut command = launch_command(&executable, &path);
        let mut child = command.spawn().map_err(|_| CatalogError::Io)?;
        // Reap the child without blocking the launcher's event loop. Launch
        // Services hands the window to another process, so there is often
        // nothing to wait for; `open` exits as soon as the app is running.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(CatalogLaunched {
            id: id.to_owned(),
            surface: surface.to_owned(),
            target: format!("fungOS {}", path.display()),
        })
    }
}

/// How one Spore opens another.
///
/// A bundled app is started through Launch Services: it gets its own activation
/// and its own session, so its window becomes key and its clicks and keys reach
/// it. A binary run from a shell has no bundle to launch, so it re-executes
/// itself — with a null stdin and stdout, because a Spore that inherits its
/// parent's terminal inherits the wrong tty along with it.
fn launch_command(executable: &std::path::Path, module: &std::path::Path) -> std::process::Command {
    if let Some(bundle) = bundle_root(executable) {
        let mut command = std::process::Command::new("/usr/bin/open");
        command.arg("-n").arg(bundle).arg("--args").arg(module);
        return command;
    }
    let mut command = std::process::Command::new(executable);
    command
        .arg(module)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

/// The `.app` an executable lives in, if it lives in one.
fn bundle_root(executable: &std::path::Path) -> Option<&std::path::Path> {
    executable.ancestors().find(|ancestor| {
        ancestor
            .extension()
            .is_some_and(|extension| extension == "app")
    })
}

struct FramePresenter {
    /// The surface extent in the Spore's layout space, kept current by the
    /// window: a Spore asks every frame, so a drag-resize reaches the layout
    /// without a second event path.
    size: Arc<Mutex<(u32, u32)>>,
    tx: mpsc::Sender<CanvasFrame>,
    window: Arc<Window>,
}

impl CanvasGpuPresenter for FramePresenter {
    fn size(&self) -> (u32, u32) {
        *self.size.lock().expect("surface size poisoned")
    }

    fn scale(&self) -> f64 {
        // The window is the only thing that knows which display it is on.
        self.window.scale_factor()
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
    input: Arc<Mutex<VecDeque<InputEvent>>>,
    /// Last surface-space pointer position, so a click can name its location.
    pointer: (f32, f32),
    /// Device pixels per surface pixel. The window is sized in points; the Spore
    /// lays out in points too, so pointer events are converted before they reach
    /// it and a click lands where the user aimed.
    scale: f32,
    /// Surface extent shared with the presenter; the window owns it.
    extent: Arc<Mutex<(u32, u32)>>,
    /// When the window opened, so the splash animation has a clock.
    opened: std::time::Instant,
    /// When the splash last painted, so it runs at a steady rate instead of
    /// spinning the event loop for the whole start-up.
    splashed: std::time::Instant,
    spores: PathBuf,
    modifiers: ModifiersState,
    started: bool,
}

impl App {
    fn push(&self, event: InputEvent) {
        self.input
            .lock()
            .expect("input queue poisoned")
            .push_back(event);
        // The guest polls with a timeout; nothing else would wake it sooner.
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Re-derive the surface extent after the window changed size.
    fn resized(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        let width = (size.width as f32 / self.scale).round() as u32;
        let height = (size.height as f32 / self.scale).round() as u32;
        if width == 0 || height == 0 {
            return;
        }
        if let Ok(mut extent) = self.extent.lock() {
            *extent = (width, height);
        }
        window.request_redraw();
    }

    fn start_component(&mut self, window: Arc<Window>) {
        if self.started {
            return;
        }
        self.started = true;
        let extent = self.extent.clone();
        let (width, height) = *extent.lock().expect("surface extent poisoned");
        eprintln!("canvas-winit: starting component at {width}x{height}");
        let module = self.module.clone();
        let tx = self.tx.clone();
        let input = self.input.clone();
        let spores = self.spores.clone();
        thread::spawn(move || {
            let result = (|| -> Result<()> {
                let bytes =
                    std::fs::read(&module).with_context(|| format!("read {}", module.display()))?;
                // A Spore that has not changed should not be compiled again:
                // the first start is seconds, every one after it is a read.
                let runtime = WebGpuRuntime::new()?.with_component_cache(component_cache(&module));
                let catalog = SporeCatalog::new(&spores)
                    .map_err(|error| anyhow::anyhow!("catalog: {error:?}"))?;
                let status = pollster::block_on(runtime.run_component_with(
                    &bytes,
                    HostSetup {
                        presenter: Box::new(FramePresenter {
                            size: extent,
                            tx,
                            window: window.clone(),
                        }),
                        input,
                        interactive: true,
                        catalog: Box::new(catalog),
                        terminal: Box::new(PtyTerminal::new(80, 24)),
                        cursor: Some(Box::new(WindowCursor(window))),
                    },
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

    /// What the window shows until the Spore's first frame arrives.
    ///
    /// Starting a Spore is not instant: a component compiles, a wgpu device
    /// comes up, and text shaping loads fonts. The mark breathes through that
    /// wait with a lissajous pair the way the fungus draws itself, so the pause
    /// reads as starting rather than as hung.
    fn paint_splash(&mut self, (width, height): (u32, u32), phase: f32) {
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let (Some(width), Some(height)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        let (width_px, height_px) = (width.get(), height.get());
        buffer.fill(BACKGROUND);
        draw_lissajous(
            &mut buffer,
            width_px,
            height_px,
            phase,
            SplashCurve {
                a: 3.0,
                b: 2.0,
                delta: std::f32::consts::FRAC_PI_2,
                radius: 0.42,
                color: ACCENT_CURVE,
            },
        );
        draw_lissajous(
            &mut buffer,
            width_px,
            height_px,
            phase,
            SplashCurve {
                a: 5.0,
                b: 4.0,
                delta: 0.0,
                radius: 0.30,
                color: ACCENT_GLOW,
            },
        );
        if let Some(logo) = logo_mark() {
            let side = 220.min(width_px).min(height_px);
            let scaled =
                image::imageops::resize(logo, side, side, image::imageops::FilterType::Lanczos3);
            let left = (width_px - side) / 2;
            let top = (height_px - side) / 2;
            for (x, y, pixel) in scaled.enumerate_pixels() {
                // The mark is an app, not a picture: mask it to the same
                // rounded square the Dock icon uses, so the window and the Dock
                // show one shape at every size.
                let coverage = squircle_coverage(x, y, side);
                if coverage <= 0.0 {
                    continue;
                }
                let Some(destination) = buffer.get_mut(((top + y) * width_px + left + x) as usize)
                else {
                    continue;
                };
                let alpha = u32::from(pixel[3]) as f32 * coverage;
                if alpha <= 0.0 {
                    continue;
                }
                let alpha = alpha.round() as u32;
                let blend = |foreground: u8, background: u8| {
                    ((u32::from(foreground) * alpha + u32::from(background) * (255 - alpha)) / 255)
                        as u8
                };
                let background = *destination;
                let r = blend(pixel[0], (background >> 16) as u8);
                let g = blend(pixel[1], (background >> 8) as u8);
                let b = blend(pixel[2], background as u8);
                *destination = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
            }
        }
        if let Err(error) = buffer.present() {
            eprintln!("canvas-winit: splash: {error}");
        }
    }

    fn paint(&mut self) {
        let (Some(surface), Some(frame), Some(window)) = (
            self.surface.as_mut(),
            self.frame.as_ref(),
            self.window.as_ref(),
        ) else {
            return;
        };
        // The window decides the resolution; the Spore decides the layout. A
        // frame rendered at a different pixel count than the window is scaled
        // here rather than cropped, so a Spore is never clipped by a display it
        // did not know about.
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        let scaled;
        let rgba: &[u8] = if (frame.width, frame.height) == (size.width, size.height) {
            frame.rgba.as_slice()
        } else {
            let Some(source) =
                image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
            else {
                return;
            };
            scaled = image::imageops::resize(
                &source,
                size.width,
                size.height,
                image::imageops::FilterType::Triangle,
            );
            scaled.as_raw().as_slice()
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
        for (pixel, rgba) in buffer.iter_mut().zip(rgba.chunks_exact(4)) {
            *pixel = (u32::from(rgba[0]) << 16) | (u32::from(rgba[1]) << 8) | u32::from(rgba[2]);
        }
        if let Err(error) = buffer.present() {
            eprintln!("canvas-winit: present: {error}");
        } else {
            eprintln!(
                "canvas-winit: painted {}x{} into {}x{}",
                frame.width, frame.height, size.width, size.height
            );
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(window_title(&self.module))
            .with_inner_size(LogicalSize::new(LOGICAL_SIZE.0, LOGICAL_SIZE.1))
            .with_resizable(true);
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

        // `surface.size()` is the coordinate space the Spore lays out in. It is
        // the logical size of the window on every display, so a layout does not
        // change shape when the window moves to a Retina screen; the presenter
        // blits whatever resolution the Spore renders at.
        self.scale = window.scale_factor() as f32;
        let inner = window.inner_size();
        if let Ok(mut extent) = self.extent.lock() {
            *extent = (
                (inner.width as f32 / self.scale).round() as u32,
                (inner.height as f32 / self.scale).round() as u32,
            );
        }
        let physical = LogicalSize::new(LOGICAL_SIZE.0, LOGICAL_SIZE.1)
            .to_physical::<u32>(window.scale_factor());
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
        self.paint_splash((physical.width, physical.height), 0.0);
        self.start_component(self.window.as_ref().unwrap().clone());
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
                if self.frame.is_some() {
                    self.paint();
                } else if let Some(window) = &self.window {
                    // Thirty frames a second is enough for a slow curve and
                    // leaves the machine to compile the Spore.
                    if self.splashed.elapsed() >= std::time::Duration::from_millis(33) {
                        self.splashed = std::time::Instant::now();
                        let size = window.inner_size();
                        let phase = self.opened.elapsed().as_secs_f32();
                        self.paint_splash((size.width, size.height), phase);
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    let key = key_name(&event.logical_key);
                    let text = event.text.as_ref().map(|text| text.to_string());
                    self.push(InputEvent::Key(InputKey {
                        key,
                        text,
                        pressed: true,
                        shift: self.modifiers.shift_key(),
                        control: self.modifiers.control_key(),
                        alt: self.modifiers.alt_key(),
                        meta: self.modifiers.super_key(),
                    }));
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor as f32;
                self.resized();
            }
            WindowEvent::Resized(_) => self.resized(),
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (
                    (position.x as f32) / self.scale,
                    (position.y as f32) / self.scale,
                );
                self.push(InputEvent::Moved {
                    x: self.pointer.0,
                    y: self.pointer.1,
                });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if state == ElementState::Pressed
                    && let Some(window) = &self.window
                {
                    // A click on a terminal means "type here". The window has to
                    // be key before any of that reaches the Spore.
                    window.focus_window();
                }
                let Some(button) = pointer_button(button) else {
                    return;
                };
                self.push(InputEvent::Button {
                    x: self.pointer.0,
                    y: self.pointer.1,
                    button,
                    pressed: state == ElementState::Pressed,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // One notch is a terminal row or a list row; a pixel-only
                // device is converted so the guest never sees raw pixels.
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, vertical) => vertical,
                    MouseScrollDelta::PixelDelta(position) => (position.y as f32) / 20.0,
                };
                self.push(InputEvent::Scrolled {
                    x: self.pointer.0,
                    y: self.pointer.1,
                    lines,
                });
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let before = self.frames_seen;
        self.take_frames(event_loop);
        if let Some(window) = &self.window
            && (self.frames_seen > before || self.frame.is_none())
        {
            // Before the first frame the splash is the only thing on screen, so
            // it keeps its own clock; afterwards a redraw means a new frame.
            window.request_redraw();
        }
    }
}

/// Where compiled Spores are kept. Outside the bundle, because a cache is not
/// part of what was installed, and keyed by module digest so a new build is a
/// new file rather than a stale hit.
fn component_cache(module: &std::path::Path) -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let base = match home {
        Some(home) => home.join("Library/Caches/dev.fungos.launcher"),
        None => module
            .parent()
            .map(|parent| parent.join(".components"))
            .unwrap_or_else(|| std::path::PathBuf::from(".components")),
    };
    base
}

/// What the window calls itself.
///
/// An installed Spore names itself in its manifest, which is the same source the
/// launcher lists it from; a bare module falls back to its file name. Either way
/// the user sees "Launcher" or "Term", never "canvas-winit — launcher.wasm".
fn window_title(module: &std::path::Path) -> String {
    let manifest = module
        .ancestors()
        .skip(1)
        .find(|directory| directory.join("spore.toml").is_file())
        .and_then(|directory| Manifest::load(directory).ok());
    if let Some(manifest) = manifest {
        if let Some(title) = manifest
            .spore
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
        {
            return title;
        }
        return title_case(&manifest.spore.name);
    }
    let stem = module
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("spore");
    title_case(stem)
}

fn title_case(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => "Spore".to_owned(),
    }
}

fn point(value: String) -> Result<(f32, f32)> {
    let (x, y) = value
        .split_once(',')
        .with_context(|| format!("{value} is not X,Y"))?;
    Ok((
        x.trim().parse().context("X is not a number")?,
        y.trim().parse().context("Y is not a number")?,
    ))
}

/// The colour a window sits on before its Spore draws anything.
const BACKGROUND: u32 = 0x0c10_15;
/// The two hues the mark draws itself with: the fungal line and its glow.
const ACCENT_CURVE: (f32, f32, f32) = (0.60, 0.80, 0.52);
const ACCENT_GLOW: (f32, f32, f32) = (0.36, 0.55, 0.46);

/// One parametric curve: x = sin(a·t + δ), y = sin(b·t).
struct SplashCurve {
    a: f32,
    b: f32,
    delta: f32,
    /// Fraction of the short side the figure spans.
    radius: f32,
    color: (f32, f32, f32),
}

fn draw_lissajous(buffer: &mut [u32], width: u32, height: u32, phase: f32, curve: SplashCurve) {
    let (half_w, half_h) = (width as f32 / 2.0, height as f32 / 2.0);
    let radius = curve.radius * width.min(height) as f32;
    let steps = 2400;
    for step in 0..steps {
        let t = step as f32 / steps as f32 * std::f32::consts::TAU + phase;
        let x = half_w + radius * (curve.a * t + curve.delta).sin();
        let y = half_h + radius * (curve.b * t).sin();
        // A 2x2 splat with a soft centre reads as a thin glowing line without a
        // rasteriser; the splash only has to hold a window for a few seconds.
        for (dx, dy, weight) in [(0i32, 0i32, 1.0f32), (1, 0, 0.5), (0, 1, 0.5), (1, 1, 0.25)] {
            let px = x as i32 + dx;
            let py = y as i32 + dy;
            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                continue;
            }
            let Some(pixel) = buffer.get_mut((py as u32 * width + px as u32) as usize) else {
                continue;
            };
            let add = |channel: u32, value: f32| {
                let current = channel as f32 / 255.0;
                ((current + value * weight * 0.12).min(1.0) * 255.0) as u32
            };
            let r = add((*pixel >> 16) & 0xff, curve.color.0);
            let g = add((*pixel >> 8) & 0xff, curve.color.1);
            let b = add(*pixel & 0xff, curve.color.2);
            *pixel = (r << 16) | (g << 8) | b;
        }
    }
}

/// The mark, decoded once: the splash redraws many times while a Spore starts.
fn logo_mark() -> Option<&'static image::RgbaImage> {
    static MARK: std::sync::OnceLock<Option<image::RgbaImage>> = std::sync::OnceLock::new();
    MARK.get_or_init(|| {
        image::load_from_memory(include_bytes!("../assets/fungos.png"))
            .ok()
            .map(|image| image.to_rgba8())
    })
    .as_ref()
}

/// Coverage of the rounded square an app icon is cut to, 0 outside and 1 well
/// inside. A superellipse would be more faithful to macOS; a rounded rect is
/// what every icon looks like at the sizes this draws.
fn squircle_coverage(x: u32, y: u32, side: u32) -> f32 {
    let side = side as f32;
    let radius = side * 0.2237;
    let half = side / 2.0;
    let px = x as f32 + 0.5 - half;
    let py = y as f32 + 0.5 - half;
    let qx = (px.abs() - (half - radius)).max(0.0);
    let qy = (py.abs() - (half - radius)).max(0.0);
    let distance = (qx * qx + qy * qy).sqrt() - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

fn pointer_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Left),
        MouseButton::Right => Some(PointerButton::Right),
        MouseButton::Middle => Some(PointerButton::Middle),
        _ => None,
    }
}

/// Applies the pointer shape a Spore asks for. The guest never sees a cursor or
/// a window; it only says what it is pointing at.
struct WindowCursor(Arc<Window>);

impl CursorHost for WindowCursor {
    fn set_cursor(&mut self, style: CursorStyle) {
        self.0.set_cursor(cursor_icon(style));
    }
}

fn cursor_icon(style: CursorStyle) -> CursorIcon {
    match style {
        CursorStyle::Default => CursorIcon::Default,
        CursorStyle::Text => CursorIcon::Text,
        CursorStyle::Pointer => CursorIcon::Pointer,
        CursorStyle::Crosshair => CursorIcon::Crosshair,
        CursorStyle::Move => CursorIcon::Move,
        CursorStyle::Grab => CursorIcon::Grab,
        CursorStyle::Grabbing => CursorIcon::Grabbing,
        CursorStyle::NoDrop => CursorIcon::NoDrop,
        CursorStyle::Wait => CursorIcon::Wait,
        CursorStyle::Help => CursorIcon::Help,
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
    let mut keys = None;
    let mut pointer = None;
    let mut click = None;
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
            "--keys" => keys = Some(args.next().context("--keys needs text")?),
            "--pointer" => pointer = Some(point(args.next().context("--pointer needs X,Y")?)?),
            "--click" => click = Some(point(args.next().context("--click needs X,Y")?)?),
            "--spores" => {
                spores = Some(PathBuf::from(
                    args.next().context("--spores needs a directory")?,
                ));
            }
            value if module.is_none() => module = Some(PathBuf::from(value)),
            _ => anyhow::bail!(
                "usage: canvas-winit <module.wasm> [--capture FILE.rgba] [--frames N] [--key KEY] [--pointer X,Y] [--click X,Y] [--spores DIR]"
            ),
        }
    }
    let module = match module {
        Some(module) => module,
        None => {
            // A .app bundle runs this binary directly; the launcher module sits
            // in Contents/Resources next to Contents/MacOS.
            let executable = std::env::current_exe().context("locate the executable")?;
            let candidate = executable
                .parent()
                .and_then(|macos| macos.parent())
                .map(|contents| contents.join("Resources/launcher.wasm"))
                .context("locate the bundle Resources directory")?;
            if !candidate.is_file() {
                anyhow::bail!(
                    "usage: canvas-winit <module.wasm> [--capture FILE.rgba] [--frames N] [--key KEY] [--pointer X,Y] [--click X,Y] [--spores DIR]"
                );
            }
            candidate
        }
    };
    let input = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(key) = key {
        let text = (key.chars().count() == 1).then(|| key.clone());
        input
            .lock()
            .expect("input queue poisoned")
            .push_back(InputEvent::Key(InputKey {
                key,
                text,
                pressed: true,
                shift: false,
                control: false,
                alt: false,
                meta: false,
            }));
    }
    if let Some(text) = keys {
        let mut queue = input.lock().expect("input queue poisoned");
        for character in text.chars() {
            let character = character.to_string();
            queue.push_back(InputEvent::Key(InputKey {
                key: character.clone(),
                text: Some(character),
                pressed: true,
                shift: false,
                control: false,
                alt: false,
                meta: false,
            }));
        }
    }
    if let Some((x, y)) = pointer {
        input
            .lock()
            .expect("input queue poisoned")
            .push_back(InputEvent::Moved { x, y });
    }
    if let Some((x, y)) = click {
        input.lock().expect("input queue poisoned").extend([
            InputEvent::Moved { x, y },
            InputEvent::Button {
                x,
                y,
                button: PointerButton::Left,
                pressed: true,
            },
            InputEvent::Button {
                x,
                y,
                button: PointerButton::Left,
                pressed: false,
            },
        ]);
    }
    let frames = frames.unwrap_or(if capture.is_some() { 1 } else { usize::MAX });
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
        frames,
        frames_seen: 0,
        input,
        pointer: (0.0, 0.0),
        scale: 1.0,
        extent: Arc::new(Mutex::new((LOGICAL_SIZE.0 as u32, LOGICAL_SIZE.1 as u32))),
        opened: std::time::Instant::now(),
        splashed: std::time::Instant::now(),
        modifiers: ModifiersState::default(),
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
