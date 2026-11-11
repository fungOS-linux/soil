//! A native window for a GPUI Spore.
//!
//! This is the smallest canvas-level wrapper: winit owns the window, the Spore
//! owns the pixels. The host runs the component in a wasmtime store, receives
//! each `soil:present` frame, and blits it to the window. No scene, no layout,
//! no hit-testing here — those belong to the Spore.

use std::{
    num::NonZeroU32,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::{Context as _, Result};
use soil_webgpu::{CanvasFrame, CanvasGpuPresenter, WebGpuRuntime};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

type SoftContext = softbuffer::Context<Arc<Window>>;
type SoftSurface = softbuffer::Surface<Arc<Window>, Arc<Window>>;

const LOGICAL_SIZE: (f64, f64) = (1000.0, 700.0);

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
        thread::spawn(move || {
            let result = (|| -> Result<()> {
                let bytes =
                    std::fs::read(&module).with_context(|| format!("read {}", module.display()))?;
                let runtime = WebGpuRuntime::new()?;
                let status = pollster::block_on(runtime.run_component_with_presenter(
                    &bytes,
                    Box::new(FramePresenter { size, tx, window }),
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
                while let Ok(frame) = self.rx.try_recv() {
                    self.frame = Some(frame);
                }
                self.paint();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let mut drew = false;
        while let Ok(frame) = self.rx.try_recv() {
            eprintln!("canvas-winit: frame {}x{}", frame.width, frame.height);
            if let Some(path) = &self.capture {
                if let Err(error) = std::fs::write(path, &frame.rgba) {
                    eprintln!("canvas-winit: write {}: {error}", path.display());
                }
                event_loop.exit();
            }
            self.frame = Some(frame);
            drew = true;
        }
        if drew && let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

fn main() -> Result<()> {
    let mut module = None;
    let mut capture = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--capture" => {
                capture = Some(PathBuf::from(
                    args.next().context("--capture needs a path")?,
                ));
            }
            value if module.is_none() => module = Some(PathBuf::from(value)),
            _ => anyhow::bail!("usage: canvas-winit <module.wasm> [--capture FILE.rgba]"),
        }
    }
    let module = module.context("usage: canvas-winit <module.wasm> [--capture FILE.rgba]")?;
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
        started: false,
    };
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    Ok(())
}
