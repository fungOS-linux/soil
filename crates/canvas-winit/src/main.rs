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
use soil_agent_host::{AgentBackend, NoAgents};
use soil_spore::Manifest;

mod terminal;

use soil_webgpu::{
    CanvasFrame, CanvasGpuPresenter, CatalogEntry, CatalogError, CatalogHost, CatalogLaunched,
    CatalogSurface, CatalogSurfaceKind, ClipboardHost, CursorHost, CursorStyle, HostSetup,
    InputEvent, InputKey, PointerButton, WebGpuRuntime,
};
use terminal::Terminals;
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
            // One unreadable package is a broken shelf, not a broken window:
            // skip it out loud, and let the Spore we were asked to run start.
            let manifest = match Manifest::load(&package) {
                Ok(manifest) => manifest,
                Err(error) => {
                    eprintln!(
                        "canvas-winit: skipping {}: {error}",
                        package.display()
                    );
                    continue;
                }
            };
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
                        .unwrap_or_else(|| soil_spore::display_title(&manifest.spore.name)),
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
        // Every Spore opens as its own app: its own bundle identity, its own
        // icon in the Dock, and therefore its own tile to come back to. Sharing
        // the launcher's identity would give every window one Dock entry.
        let mut command = std::process::Command::new("/usr/bin/open");
        match spore_bundle(bundle, module) {
            Some(target) => {
                command.arg("-n").arg(target);
            }
            None => {
                command.arg("-n").arg(bundle).arg("--args").arg(module);
            }
        }
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

/// Build (or refresh) a `.app` that stands for one Spore, and answer its path.
///
/// The bundle carries its own copy of the binary and the module. A symlink would
/// be cheaper, but macOS names a Dock tile by the executable a process actually
/// runs: through a symlink that resolves back to the launcher, so every Spore
/// showed up as "fungOS". Copies are refreshed when the source changes.
fn spore_bundle(app: &std::path::Path, module: &std::path::Path) -> Option<std::path::PathBuf> {
    let slug = module
        .parent()
        .and_then(|package| package.file_name())
        .and_then(|name| name.to_str())?
        .to_owned();
    let title = manifest_title(module).unwrap_or_else(|| soil_spore::display_title(&slug));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    let bundle = home
        .join("Library/Application Support/fungOS/apps")
        .join(format!("{slug}.app"));
    let contents = bundle.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    std::fs::create_dir_all(&macos).ok()?;
    std::fs::create_dir_all(&resources).ok()?;

    let executable = std::env::current_exe().ok()?;
    refresh_copy(&executable, &macos.join("canvas-winit"))?;
    refresh_copy(module, &resources.join("spore.wasm"))?;
    // The icon comes from the launcher bundle so every Spore is a fungOS app.
    if let Some(parent) = app.parent() {
        let icon = parent.join("Resources/fungos.icns");
        if icon.is_file() {
            let _ = std::fs::copy(&icon, resources.join("fungos.icns"));
        }
    }
    // The bundle says what it is called; the module beside it is just
    // `spore.wasm`, so the title travels with the bundle.
    let _ = std::fs::write(resources.join("title.txt"), &title);
    let identifier = format!("dev.fungos.spore.{slug}");
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n\
  <key>CFBundleExecutable</key><string>canvas-winit</string>\n\
  <key>CFBundleIdentifier</key><string>{identifier}</string>\n\
  <key>CFBundleName</key><string>{title}</string>\n\
  <key>CFBundleDisplayName</key><string>{title}</string>\n\
  <key>CFBundleIconFile</key><string>fungos</string>\n\
  <key>CFBundlePackageType</key><string>APPL</string>\n\
  <key>CFBundleShortVersionString</key><string>0.1.0</string>\n\
  <key>CFBundleVersion</key><string>0.1.0</string>\n\
  <key>LSMinimumSystemVersion</key><string>13.0</string>\n\
  <key>NSHighResolutionCapable</key><true/>\n\
</dict></plist>\n"
    );
    std::fs::write(contents.join("Info.plist"), plist).ok()?;
    Some(bundle)
}

/// Copy `source` over `destination` when the destination is missing, older, or a
/// different size. A stale copy would run yesterday's binary.
fn refresh_copy(source: &std::path::Path, destination: &std::path::Path) -> Option<()> {
    let fresh = match (
        std::fs::metadata(source).ok(),
        std::fs::metadata(destination).ok(),
    ) {
        (Some(source), Some(destination)) => {
            destination.len() == source.len()
                && destination.modified().ok() >= source.modified().ok()
        }
        _ => false,
    };
    if fresh {
        return Some(());
    }
    std::fs::copy(source, destination).ok().map(|_| ())
}

/// The display name a module's package gives itself, if it has a manifest.
fn manifest_title(module: &std::path::Path) -> Option<String> {
    let package = module.parent()?;
    let manifest = Manifest::load(package).ok()?;
    manifest
        .spore
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .or_else(|| Some(soil_spore::display_title(&manifest.spore.name)))
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
    /// The window's opening size in points. A Spore lays out in this space, so
    /// a phone-shaped Spore opens phone-shaped.
    size: (f64, f64),
    tx: mpsc::Sender<CanvasFrame>,
    rx: mpsc::Receiver<CanvasFrame>,
    window: Option<Arc<Window>>,
    context: Option<SoftContext>,
    surface: Option<SoftSurface>,
    frame: Option<CanvasFrame>,
    capture: Option<PathBuf>,
    frames: usize,
    frames_seen: usize,
    /// The cast this window is looking at. Taken once, when the Spore starts:
    /// the backend belongs to the guest's thread, not to the window.
    agents: Option<Box<dyn AgentBackend>>,
    input: Arc<Mutex<VecDeque<InputEvent>>>,
    /// Last surface-space pointer position, so a click can name its location.
    pointer: (f32, f32),
    /// Device pixels per surface pixel. The window is sized in points; the Spore
    /// lays out in points too, so pointer events are converted before they reach
    /// it and a click lands where the user aimed.
    scale: f32,
    /// Surface extent shared with the presenter; the window owns it.
    extent: Arc<Mutex<(u32, u32)>>,
    /// The size the window last reported, and when. A resize is applied once
    /// the drag goes quiet: reflowing the emulator and re-rendering the grid on
    /// every mouse-move is what makes a drag feel broken.
    pending_resize: Option<(std::time::Instant, (u32, u32))>,
    /// Which splash to draw.
    splash: Splash,
    /// When the window opened, so the splash animation has a clock.
    opened: std::time::Instant,
    /// When the splash last painted, so it runs at a steady rate instead of
    /// spinning the event loop for the whole start-up.
    splashed: std::time::Instant,
    spores: PathBuf,
    /// What runs on the terminal's pty: a login shell when absent.
    command: Option<String>,
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

    /// Bring up another launcher. It is a separate app instance, so it gets its
    /// own window without disturbing the Spore in this one.
    fn open_launcher(&mut self) {
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let mut command = match bundle_root(&executable) {
            // The launcher is the app bundle itself: no module argument means
            // "show me the Spores".
            Some(bundle) => {
                let mut command = std::process::Command::new("/usr/bin/open");
                command.arg("-n").arg(bundle);
                command
            }
            // A bare binary has no launcher bundle to open; the shortcut is
            // only meaningful for the app.
            None => return,
        };
        if let Ok(mut child) = command.spawn() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }

    /// Note the window's new size. The extent itself is committed once the drag
    /// has been still for a moment.
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
        self.pending_resize = Some((std::time::Instant::now(), (width, height)));
    }

    /// Apply a pending resize when the window has stopped moving.
    fn settle_resize(&mut self) {
        let Some((at, size)) = self.pending_resize else {
            return;
        };
        let momentarily = std::time::Duration::from_millis(120);
        if at.elapsed() < momentarily {
            return;
        }
        self.pending_resize = None;
        if let Ok(mut extent) = self.extent.lock() {
            *extent = size;
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
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
        let command = self.command.clone();
        let tx = self.tx.clone();
        let input = self.input.clone();
        let spores = self.spores.clone();
        // Built in `main`, owned by the window until the guest starts.
        let agents = self
            .agents
            .take()
            .unwrap_or_else(|| Box::new(NoAgents));
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
                        agents,
                        catalog: Box::new(catalog),
                        store_origin: Some(soil_webgpu::LOCAL_STORE_ORIGIN.to_string()),
                        terminal: Box::new(Terminals::new(80, 24, command)),
                        cursor: Some(Box::new(WindowCursor(window))),
                        clipboard: Some(Box::new(SystemClipboard)),
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
        let opened = self.opened;
        buffer.fill(BACKGROUND);
        match self.splash {
            // A small, slow figure, drawn thick enough to read as a mark rather
            // than a wireframe: no logo, nothing to wait for but motion.
            Splash::Flow => {
                // One figure that morphs: each Lissajous ratio and phase is a
                // symbol, and the mark eases from one to the next instead of
                // cutting between them.
                let symbols = [
                    (3.0f32, 2.0f32, 0.0f32),
                    (5.0, 4.0, std::f32::consts::FRAC_PI_4),
                    (2.0, 3.0, std::f32::consts::FRAC_PI_2),
                    (5.0, 2.0, std::f32::consts::FRAC_PI_3),
                    (4.0, 3.0, std::f32::consts::FRAC_PI_6),
                ];
                let hold = 1.6f32;
                let travel = opened.elapsed().as_secs_f32() / hold;
                let index = travel.floor() as usize;
                let t = travel.fract();
                let ease = t * t * (3.0 - 2.0 * t);
                let (a0, b0, d0) = symbols[index % symbols.len()];
                let (a1, b1, d1) = symbols[(index + 1) % symbols.len()];
                let lerp = |from: f32, to: f32| from + (to - from) * ease;
                draw_lissajous(
                    &mut buffer,
                    width_px,
                    height_px,
                    0.0,
                    SplashCurve {
                        a: lerp(a0, a1),
                        b: lerp(b0, b1),
                        delta: lerp(d0, d1),
                        radius: 0.24,
                        color: ACCENT_CURVE,
                        weight: 0.85,
                        spin: 0.0,
                    },
                );
            }
            Splash::Logo => {
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
                        weight: 1.0,
                        spin: 0.0,
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
                        weight: 1.0,
                        spin: 0.0,
                    },
                );
            }
        }
        if self.splash == Splash::Logo
            && let Some(logo) = logo_mark()
        {
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
            .with_inner_size(LogicalSize::new(self.size.0, self.size.1))
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
        let physical = LogicalSize::new(self.size.0, self.size.1)
            .to_physical::<u32>(window.scale_factor());
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
        self.paint_splash((physical.width, physical.height), 0.0);
        self.start_component(self.window.as_ref().unwrap().clone());
        warm_installed(&self.spores, &self.module);
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
                self.settle_resize();
                self.take_frames(event_loop);
                if self.frame.is_some() {
                    self.paint();
                } else if let Some(window) = &self.window {
                    // Sixty frames a second: the sweep is the only thing
                    // moving, and a stepping animation reads as a stutter.
                    if self.splashed.elapsed() >= std::time::Duration::from_millis(16) {
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
                if event.state == ElementState::Pressed
                    && (self.modifiers.super_key() || self.modifiers.control_key())
                    && matches!(
                        event.logical_key,
                        WinitKey::Character(ref text) if matches!(text.as_str(), "o" | "l")
                    )
                {
                    self.open_launcher();
                }
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
        self.settle_resize();
        // Come back on a clock while there is something to animate: the splash
        // before the first frame, and a resize settling. With a frame on screen
        // and nothing moving, wait for an event instead of spinning.
        let wake = if self.frame.is_none() {
            Some(std::time::Duration::from_millis(16))
        } else if self.pending_resize.is_some() {
            Some(std::time::Duration::from_millis(50))
        } else {
            None
        };
        match wake {
            Some(after) => event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + after,
            )),
            None => event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait),
        }
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

/// Compile the Spores this machine has, in the background, so opening one from
/// the launcher does not pay for it. The module already running is skipped; a
/// Spore whose cache is warm costs a file read and a deserialize to check.
fn warm_installed(spores: &std::path::Path, running: &std::path::Path) {
    let Ok(directory) = std::fs::read_dir(spores) else {
        return;
    };
    let modules: Vec<std::path::PathBuf> = directory
        .flatten()
        .map(|entry| entry.path())
        .filter(|package| package.join("spore.toml").is_file())
        .filter_map(|package| {
            let manifest = Manifest::load(&package).ok()?;
            let module = manifest.module?;
            Some(package.join(module.path))
        })
        .filter(|module| module != running)
        .collect();
    if modules.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for module in modules {
            let Ok(bytes) = std::fs::read(&module) else {
                continue;
            };
            let Ok(runtime) = WebGpuRuntime::new() else {
                continue;
            };
            let runtime = runtime.with_component_cache(component_cache(&module));
            if let Err(error) = runtime.prewarm(&bytes) {
                eprintln!("canvas-winit: warm {}: {error:#}", module.display());
            }
        }
    });
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
    // A Spore bundle names itself beside its module.
    if let Some(title) = module
        .parent()
        .map(|resources| resources.join("title.txt"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty())
    {
        return title;
    }
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
        return soil_spore::display_title(&manifest.spore.name);
    }
    let stem = module
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("spore");
    soil_spore::display_title(stem)
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

/// What the window shows while a Spore starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Splash {
    /// The mark, masked like the Dock icon, over two thin curves.
    Logo,
    /// No mark: a small, thick lissajous figure that moves.
    Flow,
}
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
    /// Line weight: 1 draws a hairline, 3 draws a stroke.
    weight: f32,
    /// Rotation of the whole figure, in radians. Changing the phase morphs a
    /// Lissajous; rotating it is what reads as spinning.
    spin: f32,
}

fn draw_lissajous(buffer: &mut [u32], width: u32, height: u32, phase: f32, curve: SplashCurve) {
    let (half_w, half_h) = (width as f32 / 2.0, height as f32 / 2.0);
    let radius = curve.radius * width.min(height) as f32;
    // A stroke, not a hairline: the figure is the only thing on screen.
    let stroke = 1.2 + curve.weight.max(0.2) * 4.0;
    let (sin, cos) = (curve.spin.sin(), curve.spin.cos());
    let point = |t: f32| {
        let x = radius * (curve.a * t + curve.delta).sin();
        let y = radius * (curve.b * t).sin();
        (half_w + x * cos - y * sin, half_h + x * sin + y * cos)
    };
    // Dense samples, each drawn as an antialiased capsule: a splat of pixels
    // would show stair-steps at this angle, and the figure is the only thing on
    // screen while a Spore starts. Enough samples for a smooth curve, few
    // enough to hold 60 fps on one core.
    let steps = 1200;
    let mut previous = point(phase);
    for step in 1..=steps {
        let t = step as f32 / steps as f32 * std::f32::consts::TAU + phase;
        let current = point(t);
        draw_capsule(
            buffer,
            width,
            height,
            previous,
            current,
            stroke,
            curve.color,
        );
        previous = current;
    }
}

/// One antialiased segment: every pixel near the line gets coverage from its
/// distance to it, and the colour is added so crossings glow.
fn draw_capsule(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    from: (f32, f32),
    to: (f32, f32),
    half_width: f32,
    color: (f32, f32, f32),
) {
    let reach = half_width + 1.0;
    let min_x = (from.0.min(to.0) - reach).floor().max(0.0) as i32;
    let max_x = (from.0.max(to.0) + reach).ceil().min(width as f32 - 1.0) as i32;
    let min_y = (from.1.min(to.1) - reach).floor().max(0.0) as i32;
    let max_y = (from.1.max(to.1) + reach).ceil().min(height as f32 - 1.0) as i32;
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length_squared = (dx * dx + dy * dy).max(f32::EPSILON);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Distance from the pixel centre to the segment.
            let t = (((px - from.0) * dx + (py - from.1) * dy) / length_squared).clamp(0.0, 1.0);
            let (cx, cy) = (from.0 + t * dx, from.1 + t * dy);
            let distance = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let coverage = (half_width + 0.5 - distance).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let Some(pixel) = buffer.get_mut((y as u32 * width + x as u32) as usize) else {
                continue;
            };
            let add = |channel: u32, value: f32| {
                let current = channel as f32 / 255.0;
                ((current + value * coverage * 0.16).min(1.0) * 255.0).round() as u32
            };
            let r = add((*pixel >> 16) & 0xff, color.0);
            let g = add((*pixel >> 8) & 0xff, color.1);
            let b = add(*pixel & 0xff, color.2);
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

fn drag_point(value: String) -> Result<((f32, f32), (f32, f32))> {
    let mut parts = value.split(',');
    let mut next = || -> Result<f32> {
        parts
            .next()
            .context("expected four numbers")?
            .trim()
            .parse()
            .context("not a number")
    };
    Ok(((next()?, next()?), (next()?, next()?)))
}

fn pointer_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Left),
        MouseButton::Right => Some(PointerButton::Right),
        MouseButton::Middle => Some(PointerButton::Middle),
        _ => None,
    }
}

/// The system pasteboard. `pbcopy` is the whole of it: the host is the only
/// thing that touches the system, and a Spore cannot see what it wrote.
struct SystemClipboard;

impl ClipboardHost for SystemClipboard {
    fn set_text(&mut self, text: String) -> Result<(), String> {
        use std::io::Write as _;
        let mut child = std::process::Command::new("/usr/bin/pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| format!("pbcopy: {error}"))?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|error| format!("pbcopy stdin: {error}"))?;
        }
        child
            .wait()
            .map(|status| status.success())
            .map_err(|error| format!("pbcopy: {error}"))?
            .then_some(())
            .ok_or_else(|| "pbcopy reported failure".to_owned())
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
    let mut command = None;
    let mut pointer = None;
    let mut click = None;
    let mut drag = None;
    let mut after: Option<u64> = None;
    let mut splash = Splash::Flow;
    let mut prewarm = false;
    let mut spores = None;
    let mut agents: Option<String> = None;
    let mut acp_cmd: Option<String> = None;
    let mut size: Option<(f64, f64)> = None;
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
            "--command" => command = Some(args.next().context("--command needs a command line")?),
            "--pointer" => pointer = Some(point(args.next().context("--pointer needs X,Y")?)?),
            "--click" => click = Some(point(args.next().context("--click needs X,Y")?)?),
            "--drag" => {
                drag = Some(drag_point(
                    args.next().context("--drag needs X1,Y1,X2,Y2")?,
                )?)
            }
            "--prewarm" => prewarm = true,
            "--splash" => {
                splash = match args.next().context("--splash needs logo or flow")?.as_str() {
                    "logo" => Splash::Logo,
                    "flow" => Splash::Flow,
                    other => anyhow::bail!("--splash takes logo or flow, not {other}"),
                }
            }
            "--after" => {
                after = Some(
                    args.next()
                        .context("--after needs milliseconds")?
                        .parse()
                        .context("--after needs a number")?,
                )
            }
            "--agents" => {
                agents = Some(args.next().context("--agents takes demo or acp")?);
            }
            "--size" => {
                let value = args.next().context("--size needs WxH")?;
                let (width, height) = value
                    .split_once('x')
                    .with_context(|| format!("--size needs WxH, not {value}"))?;
                size = Some((
                    width.parse().context("--size needs a width")?,
                    height.parse().context("--size needs a height")?,
                ));
            }
            "--acp-cmd" => {
                acp_cmd = Some(args.next().context("--acp-cmd needs a command")?);
            }
            "--spores" => {
                spores = Some(PathBuf::from(
                    args.next().context("--spores needs a directory")?,
                ));
            }
            value if module.is_none() => module = Some(PathBuf::from(value)),
            _ => anyhow::bail!(
                "usage: canvas-winit <module.wasm> [--capture FILE.rgba] [--frames N] [--key KEY] [--keys TEXT] [--command LINE] [--pointer X,Y] [--click X,Y] [--drag X1,Y1,X2,Y2] [--after MS] [--spores DIR]"
            ),
        }
    }
    let module = match module {
        Some(module) => module,
        None => {
            // A .app bundle runs this binary directly; the launcher module sits
            // in Contents/Resources next to Contents/MacOS.
            let executable = std::env::current_exe().context("locate the executable")?;
            let resources = executable
                .parent()
                .and_then(|macos| macos.parent())
                .map(|contents| contents.join("Resources"))
                .context("locate the bundle Resources directory")?;
            // A per-Spore bundle carries its module as `spore.wasm`; the
            // launcher's own bundle carries `launcher.wasm`.
            let candidate = {
                let spore = resources.join("spore.wasm");
                if spore.is_file() {
                    spore
                } else {
                    resources.join("launcher.wasm")
                }
            };
            if !candidate.is_file() {
                anyhow::bail!(
                    "usage: canvas-winit <module.wasm> [--capture FILE.rgba] [--frames N] [--key KEY] [--keys TEXT] [--command LINE] [--pointer X,Y] [--click X,Y] [--drag X1,Y1,X2,Y2] [--after MS] [--spores DIR]"
                );
            }
            candidate
        }
    };
    // Compile and cache, then exit: no window, no device, no Spore.
    if prewarm {
        let bytes = std::fs::read(&module).with_context(|| format!("read {}", module.display()))?;
        let runtime = WebGpuRuntime::new()?.with_component_cache(component_cache(&module));
        runtime.prewarm(&bytes)?;
        println!("canvas-winit: warmed {}", module.display());
        return Ok(());
    }
    let cast: Box<dyn AgentBackend> = match agents.as_deref() {
        None => Box::new(NoAgents),
        Some("demo") => Box::new(soil_agent_host::FixtureAgents::new()),
        Some("acp") => {
            #[cfg(feature = "acp")]
            {
                let command = acp_cmd.clone().unwrap_or_else(|| "delta-acp".to_string());
                Box::new(soil_webgpu::acp::AcpBackend::spawn(&command).expect("start the ACP agent"))
            }
            #[cfg(not(feature = "acp"))]
            {
                panic!("--agents acp needs the `acp` feature");
            }
        }
        Some(other) => panic!("--agents: unknown source {other:?} (demo, acp)"),
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
    // Input seeded before the window exists is delivered the moment the guest
    // starts, which is before the shell has painted anything. `--after` waits
    // for the app to be worth typing into.
    let seed_queue = input.clone();
    let seed = move || {
        let input = seed_queue;
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
        if let Some(((x1, y1), (x2, y2))) = drag {
            let steps = 8;
            let mut queue = input.lock().expect("input queue poisoned");
            queue.push_back(InputEvent::Moved { x: x1, y: y1 });
            queue.push_back(InputEvent::Button {
                x: x1,
                y: y1,
                button: PointerButton::Left,
                pressed: true,
            });
            for step in 1..=steps {
                let t = step as f32 / steps as f32;
                queue.push_back(InputEvent::Moved {
                    x: x1 + (x2 - x1) * t,
                    y: y1 + (y2 - y1) * t,
                });
            }
            queue.push_back(InputEvent::Button {
                x: x2,
                y: y2,
                button: PointerButton::Left,
                pressed: false,
            });
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
    };
    match after {
        Some(milliseconds) => {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(milliseconds));
                seed();
            });
        }
        None => seed(),
    }
    let frames = frames.unwrap_or(if capture.is_some() { 1 } else { usize::MAX });
    let (tx, rx) = mpsc::channel();
    let mut app = App {
        module,
        size: size.unwrap_or(LOGICAL_SIZE),
        tx,
        rx,
        window: None,
        context: None,
        surface: None,
        frame: None,
        capture,
        frames,
        frames_seen: 0,
        agents: Some(cast),
        input,
        pointer: (0.0, 0.0),
        scale: 1.0,
        extent: Arc::new(Mutex::new((LOGICAL_SIZE.0 as u32, LOGICAL_SIZE.1 as u32))),
        pending_resize: None,
        splash,
        opened: std::time::Instant::now(),
        splashed: std::time::Instant::now(),
        modifiers: ModifiersState::default(),
        command,
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
