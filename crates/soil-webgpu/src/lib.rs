//! Bounded `wasi:webgpu` host for Soil.
//!
//! This is intentionally a separate crate from `soil-runtime`: the Canvas
//! runtime is a core-Wasm host, while WebGPU is a component-model capability.
//! A Spore that imports `wasi:webgpu` gets exactly this linker and nothing
//! else. Adapter/device creation, validation, limits and queue submission stay
//! in the host through `wgpu-core`; no native handle crosses the boundary.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use wasi_webgpu_wasmtime::reexports::{wgpu_core, wgpu_types};
use wasi_webgpu_wasmtime::{WasiWebGpuCtx, WasiWebGpuCtxView, WasiWebGpuOptions};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use soil::present::input;

wasmtime::component::bindgen!({
    path: "wit",
    world: "runner",
    exports: {
        "run": async,
    },
    with: {
        "wasi:webgpu/webgpu@0.3.0-rc.2.gpu-texture": wasi_webgpu_wasmtime::Texture,
    },
    require_store_data_send: true,
});

/// One composed Canvas frame, tightly packed top-to-bottom RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Host-side destination for a guest `soil:present` frame.
///
/// Canvas owns the actual window and final composition; this trait only lets
/// the WebGPU host hand over a completed frame. It is never called from inside
/// a guest callback.
pub trait CanvasGpuPresenter: Send {
    fn size(&self) -> (u32, u32);

    /// Device pixels per layout pixel. A capture host renders at 1:1.
    fn scale(&self) -> f64 {
        1.0
    }

    fn present(&mut self, frame: CanvasFrame) -> Result<()>;
}

struct DiscardPresenter {
    size: (u32, u32),
}

impl CanvasGpuPresenter for DiscardPresenter {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn present(&mut self, _frame: CanvasFrame) -> Result<()> {
        Ok(())
    }
}

/// A presenter that retains exactly one frame for tests and CLI output.
#[derive(Clone, Default)]
pub struct FrameCapture {
    frame: Arc<Mutex<Option<CanvasFrame>>>,
    size: (u32, u32),
}

impl FrameCapture {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            frame: Arc::new(Mutex::new(None)),
            size: (width, height),
        }
    }

    pub fn take(&self) -> Option<CanvasFrame> {
        self.frame.lock().expect("frame capture poisoned").take()
    }
}

impl CanvasGpuPresenter for FrameCapture {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn present(&mut self, frame: CanvasFrame) -> Result<()> {
        let mut slot = self.frame.lock().expect("frame capture poisoned");
        anyhow::ensure!(slot.is_none(), "present budget");
        *slot = Some(frame);
        Ok(())
    }
}

/// One installed Spore, in the plain shape a presenter supplies. The WIT
/// records are built at the boundary; hosts do not depend on generated types.
#[derive(Clone, Debug)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    /// Display label; the host resolves the manifest's `title` or title-cases
    /// `name`, so every front door agrees on what an app is called.
    pub title: String,
    pub version: String,
    pub summary: String,
    pub workspace: Option<String>,
    pub default_surface: String,
    pub surfaces: Vec<CatalogSurface>,
}

#[derive(Clone, Debug)]
pub struct CatalogSurface {
    pub name: String,
    pub kind: CatalogSurfaceKind,
    pub entry: String,
    pub requires: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogSurfaceKind {
    Canvas,
    Web,
}

#[derive(Clone, Debug)]
pub struct CatalogLaunched {
    pub id: String,
    pub surface: String,
    pub target: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogError {
    Denied,
    NotFound,
    Invalid,
    Unsupported,
    Io,
}

/// The host's installed-Spore catalogue. `launch` is host-owned: the guest
/// asks, the host starts the process or opens the window.
pub trait CatalogHost: Send {
    fn entries(&mut self) -> Result<Vec<CatalogEntry>, CatalogError>;
    fn launch(&mut self, id: &str, surface: &str) -> Result<CatalogLaunched, CatalogError>;
}

struct NoCatalog;

impl CatalogHost for NoCatalog {
    fn entries(&mut self) -> Result<Vec<CatalogEntry>, CatalogError> {
        Ok(Vec::new())
    }

    fn launch(&mut self, _id: &str, _surface: &str) -> Result<CatalogLaunched, CatalogError> {
        Err(CatalogError::NotFound)
    }
}

/// One terminal cell, in the plain shape the host's emulator publishes.
#[derive(Clone, Debug, Default)]
pub struct TerminalCell {
    pub text: String,
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub faint: bool,
    pub inverse: bool,
    pub wide_tail: bool,
}

#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    pub cols: u32,
    pub rows: u32,
    pub cells: Vec<TerminalCell>,
    pub cursor: Option<(u32, u32)>,
    pub title: String,
    pub generation: u64,
}

/// The host's live terminal. The Spore asks for a snapshot and sends keys;
/// it never owns a PTY, a shell, or an emulator.
pub trait TerminalHost: Send {
    fn snapshot(&mut self) -> Result<TerminalSnapshot, String>;
    fn write(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn resize(&mut self, cols: u32, rows: u32) -> Result<(), String>;
    /// Move through scrollback. Negative is towards history.
    fn scroll(&mut self, delta: i32) -> Result<(), String>;
}

struct NoTerminal;

impl TerminalHost for NoTerminal {
    fn snapshot(&mut self) -> Result<TerminalSnapshot, String> {
        Err("no terminal session attached".into())
    }

    fn write(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("no terminal session attached".into())
    }

    fn resize(&mut self, _cols: u32, _rows: u32) -> Result<(), String> {
        Err("no terminal session attached".into())
    }

    fn scroll(&mut self, _delta: i32) -> Result<(), String> {
        Err("no terminal session attached".into())
    }
}

/// The pointer shape a Spore asks for. A presenter maps it to a native cursor;
/// a headless host drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Default,
    Text,
    Pointer,
    Crosshair,
    Move,
    Grab,
    Grabbing,
    NoDrop,
    Wait,
    Help,
}

/// The window's pointer shape. Presenters that own a window implement it;
/// a capture or headless host leaves it unset and `set` succeeds doing nothing.
pub trait CursorHost: Send {
    fn set_cursor(&mut self, style: CursorStyle);
}

/// One normalized key waiting for the guest. The WIT type is built at the
/// boundary; presenters only need this plain shape.
#[derive(Clone, Debug)]
pub struct InputKey {
    pub key: String,
    pub text: Option<String>,
    pub pressed: bool,
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

/// Presenter-independent pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

/// One event waiting for the guest, in the order the presenter produced it.
/// Keys and pointer actions share a queue, so a click cannot overtake a
/// keystroke that arrived before it.
#[derive(Clone, Debug)]
pub enum InputEvent {
    Key(InputKey),
    /// Surface-space position: the units `soil:present/surface.size` reports.
    Moved {
        x: f32,
        y: f32,
    },
    Button {
        x: f32,
        y: f32,
        button: PointerButton,
        pressed: bool,
    },
    /// Positive `lines` scrolls towards earlier content.
    Scrolled {
        x: f32,
        y: f32,
        lines: f32,
    },
}

/// Host state for one component instance.
pub struct WebGpuHost {
    instance: Arc<wgpu_core::global::Global>,
    table: ResourceTable,
    options: WasiWebGpuOptions,
    wasi: WasiCtx,
    presenter: Box<dyn CanvasGpuPresenter>,
    input: Arc<Mutex<VecDeque<InputEvent>>>,
    interactive: bool,
    catalog: Box<dyn CatalogHost>,
    terminal: Box<dyn TerminalHost>,
    cursor: Option<Box<dyn CursorHost>>,
}

/// Everything one Spore run is allowed to talk to. A presenter fills in the
/// parts it owns; anything left as the denying default refuses the guest loudly
/// instead of quietly doing nothing.
pub struct HostSetup {
    pub presenter: Box<dyn CanvasGpuPresenter>,
    pub input: Arc<Mutex<VecDeque<InputEvent>>>,
    /// Tells the guest to keep polling after the first frame.
    pub interactive: bool,
    pub catalog: Box<dyn CatalogHost>,
    pub terminal: Box<dyn TerminalHost>,
    /// `None` on hosts without a window; `soil:present/cursor` still succeeds.
    pub cursor: Option<Box<dyn CursorHost>>,
}

impl HostSetup {
    /// A one-shot capture host: one frame, no input, no catalog, no terminal.
    pub fn capture(presenter: Box<dyn CanvasGpuPresenter>) -> Self {
        Self {
            presenter,
            input: Arc::new(Mutex::new(VecDeque::new())),
            interactive: false,
            catalog: Box::new(NoCatalog),
            terminal: Box::new(NoTerminal),
            cursor: None,
        }
    }
}

impl WebGpuHost {
    pub fn new(setup: HostSetup) -> Result<Self> {
        let descriptor = wgpu_types::InstanceDescriptor {
            backends: wgpu_types::Backends::all(),
            flags: wgpu_types::InstanceFlags::from_build_config(),
            backend_options: Default::default(),
            memory_budget_thresholds: Default::default(),
            display: None,
        };
        let instance = wgpu_core::global::Global::new("webgpu", descriptor, None);
        Ok(Self {
            instance: Arc::new(instance),
            table: ResourceTable::new(),
            options: WasiWebGpuOptions::default(),
            wasi: WasiCtxBuilder::new().build(),
            presenter: setup.presenter,
            input: setup.input,
            interactive: setup.interactive,
            catalog: setup.catalog,
            terminal: setup.terminal,
            cursor: setup.cursor,
        })
    }
}

impl soil::present::input::Host for WebGpuHost {
    fn interactive(&mut self) -> bool {
        self.interactive
    }

    fn poll(&mut self, timeout_ms: u32) -> Option<input::Event> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        loop {
            if let Some(event) = self.input.lock().expect("input queue poisoned").pop_front() {
                return Some(match event {
                    InputEvent::Key(key) => input::Event::KeyInput(input::Key {
                        key: key.key,
                        text: key.text,
                        pressed: key.pressed,
                        shift: key.shift,
                        control: key.control,
                        alt: key.alt,
                        meta: key.meta,
                    }),
                    InputEvent::Moved { x, y } => input::Event::Moved(input::PointerMove { x, y }),
                    InputEvent::Button {
                        x,
                        y,
                        button,
                        pressed,
                    } => input::Event::Button(input::PointerPress {
                        x,
                        y,
                        button: match button {
                            PointerButton::Left => input::PointerButton::Left,
                            PointerButton::Right => input::PointerButton::Right,
                            PointerButton::Middle => input::PointerButton::Middle,
                        },
                        pressed,
                    }),
                    InputEvent::Scrolled { x, y, lines } => {
                        input::Event::Scrolled(input::PointerScroll { x, y, lines })
                    }
                });
            }
            if !self.interactive || timeout_ms == 0 || Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl WasiView for WebGpuHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl soil::present::surface::Host for WebGpuHost {
    fn size(&mut self) -> (u32, u32) {
        self.presenter.size()
    }

    fn scale(&mut self) -> f64 {
        self.presenter.scale()
    }

    fn present(
        &mut self,
        texture: wasmtime::component::Resource<wasi_webgpu_wasmtime::Texture>,
    ) -> std::result::Result<(), String> {
        let image = wasi_webgpu_wasmtime::read_texture_rgba(&self.instance, &self.table, texture)
            .map_err(|e| e.to_string())?;
        let frame = CanvasFrame {
            width: image.width,
            height: image.height,
            rgba: image.rgba,
        };
        self.presenter.present(frame).map_err(|e| e.to_string())
    }
}

impl soil::spore::catalog::Host for WebGpuHost {
    fn entries(
        &mut self,
    ) -> std::result::Result<Vec<soil::spore::catalog::Entry>, soil::spore::catalog::Error> {
        let entries = self.catalog.entries().map_err(catalog_error)?;
        Ok(entries
            .into_iter()
            .map(|entry| soil::spore::catalog::Entry {
                id: entry.id,
                name: entry.name,
                title: entry.title,
                version: entry.version,
                summary: entry.summary,
                workspace: entry.workspace,
                default_surface: entry.default_surface,
                surfaces: entry
                    .surfaces
                    .into_iter()
                    .map(|surface| soil::spore::catalog::Surface {
                        name: surface.name,
                        kind: match surface.kind {
                            CatalogSurfaceKind::Canvas => soil::spore::catalog::SurfaceKind::Canvas,
                            CatalogSurfaceKind::Web => soil::spore::catalog::SurfaceKind::Web,
                        },
                        entry: surface.entry,
                        requires: surface.requires,
                    })
                    .collect(),
            })
            .collect())
    }

    fn launch(
        &mut self,
        id: String,
        surface: String,
    ) -> std::result::Result<soil::spore::catalog::Launched, soil::spore::catalog::Error> {
        let launched = self.catalog.launch(&id, &surface).map_err(catalog_error)?;
        Ok(soil::spore::catalog::Launched {
            id: launched.id,
            surface: launched.surface,
            target: launched.target,
        })
    }
}

fn catalog_error(error: CatalogError) -> soil::spore::catalog::Error {
    match error {
        CatalogError::Denied => soil::spore::catalog::Error::Denied,
        CatalogError::NotFound => soil::spore::catalog::Error::NotFound,
        CatalogError::Invalid => soil::spore::catalog::Error::Invalid,
        CatalogError::Unsupported => soil::spore::catalog::Error::Unsupported,
        CatalogError::Io => soil::spore::catalog::Error::Io,
    }
}

impl soil::terminal_screen::screen::Host for WebGpuHost {
    fn snapshot(&mut self) -> std::result::Result<soil::terminal_screen::screen::Grid, String> {
        let snapshot = self.terminal.snapshot()?;
        Ok(soil::terminal_screen::screen::Grid {
            cols: snapshot.cols,
            rows: snapshot.rows,
            cells: snapshot
                .cells
                .into_iter()
                .map(|cell| soil::terminal_screen::screen::Cell {
                    text: cell.text,
                    fg: cell.fg,
                    bg: cell.bg,
                    bold: cell.bold,
                    italic: cell.italic,
                    underline: cell.underline,
                    faint: cell.faint,
                    inverse: cell.inverse,
                    wide_tail: cell.wide_tail,
                })
                .collect(),
            cursor_row: snapshot.cursor.map(|cursor| cursor.0),
            cursor_col: snapshot.cursor.map(|cursor| cursor.1),
            title: snapshot.title,
            generation: snapshot.generation,
        })
    }

    fn write(&mut self, bytes: Vec<u8>) -> std::result::Result<(), String> {
        self.terminal.write(&bytes)
    }

    fn scroll(&mut self, delta: i32) -> std::result::Result<(), String> {
        self.terminal.scroll(delta)
    }

    fn resize(&mut self, cols: u32, rows: u32) -> std::result::Result<(), String> {
        self.terminal.resize(cols, rows)
    }
}

impl soil::present::cursor::Host for WebGpuHost {
    fn set(
        &mut self,
        style: soil::present::cursor::CursorStyle,
    ) -> std::result::Result<(), String> {
        let style = match style {
            soil::present::cursor::CursorStyle::Default => CursorStyle::Default,
            soil::present::cursor::CursorStyle::Text => CursorStyle::Text,
            soil::present::cursor::CursorStyle::Pointer => CursorStyle::Pointer,
            soil::present::cursor::CursorStyle::Crosshair => CursorStyle::Crosshair,
            soil::present::cursor::CursorStyle::Move => CursorStyle::Move,
            soil::present::cursor::CursorStyle::Grab => CursorStyle::Grab,
            soil::present::cursor::CursorStyle::Grabbing => CursorStyle::Grabbing,
            soil::present::cursor::CursorStyle::NoDrop => CursorStyle::NoDrop,
            soil::present::cursor::CursorStyle::Wait => CursorStyle::Wait,
            soil::present::cursor::CursorStyle::Help => CursorStyle::Help,
        };
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.set_cursor(style);
        }
        Ok(())
    }
}

impl WasiWebGpuCtxView for WebGpuHost {
    fn webgpu_ctx(&mut self) -> WasiWebGpuCtx<'_> {
        WasiWebGpuCtx {
            instance: &self.instance,
            table: &mut self.table,
            options: &self.options,
        }
    }
}

/// A reusable engine/linker pair. Each invocation gets a fresh store and
/// resource table, so two guests never share handles or validation state.
pub struct WebGpuRuntime {
    engine: Engine,
    linker: Linker<WebGpuHost>,
}

impl WebGpuRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.wasm_component_model_async(true);
        let engine = Engine::new(&config).map_err(anyhow::Error::from)?;
        let mut linker = Linker::new(&engine);
        // `wasi:io/poll` is the transport for component-model async, not an
        // authority grant. The context has no preopens, env, args, or network
        // policy; the guest only imports poll.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(anyhow::Error::from)
            .context("link wasi:io polling")?;
        wasi_webgpu_wasmtime::add_to_linker(&mut linker)
            .map_err(anyhow::Error::from)
            .context("link wasi:webgpu")?;
        soil::present::surface::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:present")?;
        soil::present::input::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:present input")?;
        soil::present::cursor::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:present cursor")?;
        soil::spore::catalog::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:spore catalog")?;
        soil::terminal_screen::screen::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:terminal-screen")?;
        Ok(Self { engine, linker })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn linker(&self) -> &Linker<WebGpuHost> {
        &self.linker
    }

    /// Compile and run one guest component. The guest must export
    /// `run: async func() -> u32`; all other imports are denied by the linker.
    pub async fn run_component(&self, bytes: &[u8]) -> Result<u32> {
        self.run_component_with(
            bytes,
            HostSetup::capture(Box::new(DiscardPresenter { size: (480, 640) })),
        )
        .await
    }

    pub async fn run_component_with_presenter(
        &self,
        bytes: &[u8],
        presenter: Box<dyn CanvasGpuPresenter>,
    ) -> Result<u32> {
        self.run_component_with(bytes, HostSetup::capture(presenter))
            .await
    }

    /// Run one guest against a fully specified host.
    pub async fn run_component_with(&self, bytes: &[u8], setup: HostSetup) -> Result<u32> {
        let component = Component::new(&self.engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile WebGPU component")?;
        let mut store = Store::new(&self.engine, WebGpuHost::new(setup)?);
        let instance = Runner::instantiate_async(&mut store, &component, &self.linker)
            .await
            .map_err(anyhow::Error::from)
            .context("instantiate WebGPU component")?;
        instance
            .func_run()
            .call_async(&mut store, ())
            .await
            .map_err(anyhow::Error::from)
            .context("run WebGPU component")
            .map(|(status,)| status)
    }
}

/// Synchronous convenience wrapper for CLI/integration tests. The actual host
/// calls remain async internally so `wasi:webgpu` request/map futures work.
pub fn run_component_blocking(bytes: &[u8]) -> Result<u32> {
    let runtime = WebGpuRuntime::new()?;
    pollster::block_on(runtime.run_component(bytes))
}

/// Run with a capture sink and return both guest status and the first frame.
pub fn run_component_capture(
    bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<(u32, Option<CanvasFrame>)> {
    let runtime = WebGpuRuntime::new()?;
    let capture = FrameCapture::new(width, height);
    let status =
        pollster::block_on(runtime.run_component_with_presenter(bytes, Box::new(capture.clone())))?;
    Ok((status, capture.take()))
}

#[cfg(test)]
mod tests {
    const WEBGPU_WIT: &str = include_str!("../wit/deps/wasi-webgpu-0.3.0-rc.2/package.wit");
    const RUNNER_WIT: &str = include_str!("../wit/runner.wit");

    #[test]
    fn pins_the_upstream_wasi_webgpu_contract() {
        assert!(WEBGPU_WIT.starts_with("package wasi:webgpu@0.3.0-rc.2;"));
        assert!(RUNNER_WIT.contains("include wasi:webgpu/imports@0.3.0-rc.2;"));
        assert!(RUNNER_WIT.contains("export run: async func() -> u32;"));
    }
}
