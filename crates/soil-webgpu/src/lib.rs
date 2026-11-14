//! Bounded `wasi:webgpu` host for Soil.
//!
//! This is intentionally a separate crate from `soil-runtime`: the Canvas
//! runtime is a core-Wasm host, while WebGPU is a component-model capability.
//! A Spore that imports `wasi:webgpu` gets exactly this linker and the
//! capabilities a presenter grants it. Adapter/device creation, validation,
//! limits and queue submission stay in the host through `wgpu-core`; no native
//! handle crosses the boundary.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ::soil::principal_host::PrincipalHost;
use ::soil::vfs_host::VfsHost;
use ::soil::http_host::{
    self, CredentialProvider, DirectTransport, HttpPolicy, HttpTransport, NoCredentials,
    Request as HttpRequest,
};
use ::soil::records_host::{self, Namespace, NamespacedRecordsHost};
use wasi_webgpu_wasmtime::reexports::{wgpu_core, wgpu_types};
use wasi_webgpu_wasmtime::{WasiWebGpuCtx, WasiWebGpuCtxView, WasiWebGpuOptions};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::soil::present::input;
use soil_agent_host::{AgentBackend, Answer, Binding, Page};

pub mod fb;

/// The denying agent backend, for presenters that own no agent session.
pub use soil_agent_host::NoAgents;

wasmtime::component::bindgen!({
    path: "wit",
    world: "runner",
    exports: {
        "run": async,
    },
    with: {
        "wasi:webgpu/webgpu@0.3.0-rc.2.gpu-texture": wasi_webgpu_wasmtime::Texture,
        // Same WIT package, one binding: the runner answers with the host
        // crate's own `soil:vfs` and `soil:principal` types rather than a
        // second generated copy that would need converting field by field.
        "soil:vfs/vfs@1.0.0": ::soil::vfs_host::vfs,
        "soil:principal/principal@0.1.0": ::soil::principal_host::api,
        // A subscription is host state, not a generated shadow type: the
        // backend keeps the position, so two subscriptions cannot disagree.
        "soil:agents/observe.subscription": AgentSubscription,
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
    /// The package's `[presentation] mark`, as SVG source. A launcher draws it;
    /// a host with no renderer ignores it.
    pub mark: Option<String>,
}

/// How large a mark may be. A tile is 104 px; anything that needs more than this
/// is not an icon, and shipping it through the catalogue would be a bug.
pub const MARK_LIMIT: usize = 64 * 1024;

/// Read a package's mark, if it declares one and ships it.
pub fn read_mark(root: &std::path::Path, manifest: &soil_spore::Manifest) -> Option<String> {
    let path = manifest.presentation.as_ref()?.mark.as_deref()?;
    let svg = std::fs::read_to_string(root.join(path)).ok()?;
    (svg.len() <= MARK_LIMIT && svg.contains("<svg")).then_some(svg)
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

/// The host's installed-Spore catalogue, read from a directory tree.
///
/// The host decides where Spores live and this answers with what is there. A
/// tree whose manifest does not parse is skipped rather than fatal: one broken
/// Spore must not empty a launcher or a storefront. `launch` is deliberately
/// unfinished — starting a window is the host's business, and a capture host
/// has nowhere to put one.
pub struct InstallCatalog {
    /// Every directory a Spore can live in, most specific first. A host joins
    /// them rather than choosing one, because a launcher that shows only what
    /// shipped or only what was installed is a launcher that lies about the
    /// machine: the same host may have a bundled set, store installs, and a
    /// project tree all at once.
    roots: Vec<std::path::PathBuf>,
}

impl InstallCatalog {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            roots: vec![root.into()],
        }
    }

    /// Join several directories, most specific first: an id found in an earlier
    /// root wins, so an install shadows a bundled copy of the same Spore.
    pub fn with_roots(roots: Vec<std::path::PathBuf>) -> Self {
        Self { roots }
    }
}

impl CatalogHost for InstallCatalog {
    fn entries(&mut self) -> Result<Vec<CatalogEntry>, CatalogError> {
        let mut found = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for root in &self.roots {
            let Ok(read) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in read.flatten() {
                let dir = entry.path();
                if !dir.join("spore.toml").is_file() {
                    continue;
                }
                let Ok(manifest) = soil_spore::Manifest::load(&dir) else {
                    continue;
                };
                if !seen.insert(manifest.spore.id.clone()) {
                    continue;
                }
                let surfaces = if manifest.surfaces.is_empty() {
                    manifest
                        .launch
                        .surfaces
                        .iter()
                        .filter_map(|kind| surface_kind(kind))
                        .map(|kind| CatalogSurface {
                            name: kind_name(kind).to_owned(),
                            kind,
                            entry: String::new(),
                            requires: None,
                        })
                        .collect()
                } else {
                    manifest
                        .surfaces
                        .iter()
                        .filter_map(|surface| {
                            Some(CatalogSurface {
                                name: surface.name.clone(),
                                kind: surface_kind(surface.kind.as_str())?,
                                entry: surface
                                    .entry(manifest.ui.as_ref())
                                    .unwrap_or_default()
                                    .to_owned(),
                                requires: surface.requires.clone(),
                            })
                        })
                        .collect()
                };
                found.push(CatalogEntry {
                    id: manifest.spore.id.clone(),
                    title: soil_spore::display_title(&manifest.spore.name),
                    name: manifest.spore.name.clone(),
                    version: manifest.spore.version.clone(),
                    summary: manifest.spore.summary.clone(),
                    workspace: manifest.launch.canvas_module.clone(),
                    default_surface: manifest.launch.default.clone(),
                    surfaces,
                    mark: read_mark(&dir, &manifest),
                });
            }
        }
        found.sort_by(|left, right| left.title.to_lowercase().cmp(&right.title.to_lowercase()));
        Ok(found)
    }

    fn launch(&mut self, _id: &str, _surface: &str) -> Result<CatalogLaunched, CatalogError> {
        Err(CatalogError::Unsupported)
    }
}

fn surface_kind(value: &str) -> Option<CatalogSurfaceKind> {
    match value {
        "canvas" => Some(CatalogSurfaceKind::Canvas),
        "web" => Some(CatalogSurfaceKind::Web),
        _ => None,
    }
}

fn kind_name(kind: CatalogSurfaceKind) -> &'static str {
    match kind {
        CatalogSurfaceKind::Canvas => "canvas",
        CatalogSurfaceKind::Web => "web",
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

/// One shell the host is running. The Spore draws these as tabs.
#[derive(Clone, Debug)]
pub struct TerminalSession {
    pub id: u32,
    pub title: String,
    pub active: bool,
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
    /// One shell the host is running, as a tab strip needs it.
    fn sessions(&mut self) -> Result<Vec<TerminalSession>, String> {
        Ok(Vec::new())
    }

    /// Start a shell and make it the active one.
    fn open_session(&mut self, _command: Option<String>) -> Result<u32, String> {
        Err("this host cannot open sessions".into())
    }

    fn select_session(&mut self, _id: u32) -> Result<(), String> {
        Err("this host cannot select sessions".into())
    }

    fn close_session(&mut self, _id: u32) -> Result<(), String> {
        Err("this host cannot close sessions".into())
    }

    fn snapshot(&mut self) -> Result<TerminalSnapshot, String>;
    fn write(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn resize(&mut self, cols: u32, rows: u32) -> Result<(), String>;
    /// Move through scrollback. Negative is towards history.
    fn scroll(&mut self, delta: i32) -> Result<(), String>;

    /// Wait until the grid moves past `after` or `timeout_ms` passes, and answer
    /// the generation that is current then.
    fn wait(&mut self, after: u64, timeout_ms: u32) -> Result<u64, String>;
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

    fn wait(&mut self, _after: u64, _timeout_ms: u32) -> Result<u64, String> {
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

/// The system clipboard, when the presenter has one. A host without a pasteboard
/// leaves it unset and `soil:present/clipboard` refuses loudly.
pub trait ClipboardHost: Send {
    fn set_text(&mut self, text: String) -> Result<(), String>;
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
    /// The HTTP policy, credentials and transport a guest may use. `None`
    /// refuses every request instead of quietly reaching the network.
    http: Option<HttpAccess>,
    /// A namespaced records book the guest may read or write, when granted.
    records: Option<NamespacedRecordsHost>,
    /// One workspace the guest may list and read. `None` denies `soil:vfs`.
    vfs: Option<VfsHost>,
    /// Who the guest is acting as. `None` denies `soil:principal`.
    principal: Option<PrincipalHost>,
    /// The cast of agents this run can see. `NoAgents` refuses every read, so a
    /// host that forgot to attach an adapter fails loudly instead of drawing an
    /// empty wall.
    agents: Box<dyn AgentBackend>,
    terminal: Box<dyn TerminalHost>,
    cursor: Option<Box<dyn CursorHost>>,
    clipboard: Option<Box<dyn ClipboardHost>>,
}

/// Where a local `soil-store serve` listens. A desktop shell that runs the
/// storefront has to allow this origin; a capture host that needs no network
/// leaves it out.
pub const LOCAL_STORE_ORIGIN: &str = "http://127.0.0.1:8800";

/// The HTTP capability one Spore run may use.
///
/// The policy, credentials and transport are host objects; the guest only
/// names a URL and an opaque credential handle. `soil-host` owns the single
/// implementation of what that request is allowed to do.
#[derive(Clone)]
pub struct HttpAccess {
    policy: HttpPolicy,
    credentials: Arc<dyn CredentialProvider>,
    transport: Arc<dyn HttpTransport>,
}

impl HttpAccess {
    pub fn new(policy: HttpPolicy) -> Self {
        Self {
            policy,
            credentials: Arc::new(NoCredentials),
            transport: Arc::new(DirectTransport),
        }
    }

    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = credentials;
        self
    }

    pub fn with_transport(mut self, transport: Arc<dyn HttpTransport>) -> Self {
        self.transport = transport;
        self
    }

    /// A GET-only grant for one origin: what the storefront browser needs.
    pub fn store(origin: impl Into<String>) -> Result<Self> {
        let policy = HttpPolicy::new(
            [origin.into()],
            16 * 1024 * 1024,
            std::time::Duration::from_secs(30),
        )?;
        Ok(Self::new(policy))
    }
}

/// A records root plus the named books a Spore may see inside it.
#[derive(Clone, Debug)]
pub struct RecordsAccess {
    pub root: PathBuf,
    pub namespaces: Vec<Namespace>,
}

impl RecordsAccess {
    /// One named book collection a Spore may read and write.
    ///
    /// This is what a document-shaped Spore asks for: the guest names the
    /// collection, and the grant decides the book it maps onto.
    pub fn book(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        book: impl Into<String>,
        collection: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            namespaces: vec![Namespace::write(name.into(), book.into(), collection.into())],
        }
    }

    /// The read-only namespace the vitals renderer uses.
    pub fn vitals(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            namespaces: vec![Namespace::read(
                "vitals.view",
                "dev/personal/telemetry",
                "vitals.view",
            )],
        }
    }
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
    /// The HTTP grant. `None` denies every request: a capture host that does
    /// not need the network has none.
    pub http: Option<HttpAccess>,
    /// The records grant. `None` refuses every records call.
    pub records: Option<RecordsAccess>,
    /// The VFS grant: one workspace the guest may list and read. `None` denies
    /// `soil:vfs`, which is the honest answer for a Spore that only draws.
    pub vfs: Option<PathBuf>,
    /// The principal grant. `None` denies `soil:principal`: a host that cannot
    /// attest who is acting must not let a guest guess.
    pub principal: Option<PrincipalHost>,
    pub agents: Box<dyn AgentBackend>,
    /// Shorthand for a read-only `soil:http` grant for one origin, folded into
    /// `http` when that is unset. The storefront browser names its own origin
    /// this way.
    pub store_origin: Option<String>,
    pub terminal: Box<dyn TerminalHost>,
    /// `None` on hosts without a window; `soil:present/cursor` still succeeds.
    pub cursor: Option<Box<dyn CursorHost>>,
    /// `None` on hosts without a pasteboard.
    pub clipboard: Option<Box<dyn ClipboardHost>>,
}

impl HostSetup {
    /// A one-shot capture host: one frame, no input, no catalog, no terminal.
    pub fn capture(presenter: Box<dyn CanvasGpuPresenter>) -> Self {
        Self {
            presenter,
            input: Arc::new(Mutex::new(VecDeque::new())),
            interactive: false,
            catalog: Box::new(NoCatalog),
            http: None,
            records: None,
            vfs: None,
            principal: None,
            agents: Box::new(NoAgents),
            store_origin: None,
            terminal: Box::new(NoTerminal),
            cursor: None,
            clipboard: None,
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
        // One HTTP implementation, two ways in: a full policy from the host, or
        // the single-origin shorthand the storefront browser uses.
        let http = match (setup.http, setup.store_origin.as_deref()) {
            (Some(http), _) => Some(http),
            (None, Some(origin)) => Some(HttpAccess::store(origin)?),
            (None, None) => None,
        };
        Ok(Self {
            instance: Arc::new(instance),
            table: ResourceTable::new(),
            options: WasiWebGpuOptions::default(),
            wasi: WasiCtxBuilder::new().build(),
            presenter: setup.presenter,
            input: setup.input,
            interactive: setup.interactive,
            catalog: setup.catalog,
            http,
            records: setup
                .records
                .map(|records| {
                    NamespacedRecordsHost::open(&records.root, records.namespaces)
                        .context("open records for the WebGPU guest")
                })
                .transpose()?,
            vfs: setup
                .vfs
                .map(|root| VfsHost::open(&root).context("open the workspace for soil:vfs"))
                .transpose()?,
            principal: setup.principal,
            agents: setup.agents,
            terminal: setup.terminal,
            cursor: setup.cursor,
            clipboard: setup.clipboard,
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

impl ::soil::vfs_host::vfs::Host for WebGpuHost {
    fn ls(
        &mut self,
        parent: String,
        offset: u64,
        limit: u32,
    ) -> std::result::Result<::soil::vfs_host::vfs::Page, ::soil::vfs_host::vfs::Error>
    {
        self.vfs
            .as_mut()
            .ok_or(::soil::vfs_host::vfs::Error::Denied)?
            .ls(parent, offset, limit)
    }

    fn stat(
        &mut self,
        path: String,
    ) -> std::result::Result<::soil::vfs_host::vfs::Entry, ::soil::vfs_host::vfs::Error>
    {
        self.vfs
            .as_mut()
            .ok_or(::soil::vfs_host::vfs::Error::Denied)?
            .stat(path)
    }

    fn read(
        &mut self,
        path: String,
        offset: u64,
        length: u32,
    ) -> std::result::Result<::soil::vfs_host::vfs::Chunk, ::soil::vfs_host::vfs::Error>
    {
        self.vfs
            .as_mut()
            .ok_or(::soil::vfs_host::vfs::Error::Denied)?
            .read(path, offset, length)
    }
}

impl ::soil::principal_host::api::Host for WebGpuHost {
    fn current(
        &mut self,
    ) -> std::result::Result<
        ::soil::principal_host::api::Identity,
        ::soil::principal_host::api::Error,
    > {
        self.principal
            .as_mut()
            .ok_or(::soil::principal_host::api::Error::Denied)?
            .current()
    }

    fn signing_key(
        &mut self,
    ) -> std::result::Result<
        Option<::soil::principal_host::api::SigningKeyInfo>,
        ::soil::principal_host::api::Error,
    > {
        self.principal
            .as_mut()
            .ok_or(::soil::principal_host::api::Error::Denied)?
            .signing_key()
    }
}

impl records_host::records::Host for WebGpuHost {
    fn get(
        &mut self,
        key: records_host::records::Key,
    ) -> std::result::Result<records_host::records::Entry, records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(records_host::records::Error::Unsupported)?
            .get(key)
    }

    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> std::result::Result<records_host::records::Page, records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(records_host::records::Error::Unsupported)?
            .scan(collection, cursor, limit)
    }

    fn upsert(
        &mut self,
        entry: records_host::records::Entry,
    ) -> std::result::Result<records_host::records::Entry, records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(records_host::records::Error::Unsupported)?
            .upsert(entry)
    }

    fn compare_and_swap(
        &mut self,
        key: records_host::records::Key,
        expected: Option<records_host::records::Version>,
        entry: records_host::records::Entry,
    ) -> std::result::Result<records_host::records::CasResult, records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(records_host::records::Error::Unsupported)?
            .compare_and_swap(key, expected, entry)
    }

    fn delete(
        &mut self,
        key: records_host::records::Key,
    ) -> std::result::Result<(), records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(records_host::records::Error::Unsupported)?
            .delete(key)
    }

    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> std::result::Result<records_host::records::Page, records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(records_host::records::Error::Unsupported)?
            .changes(since, limit)
    }

    fn apply(
        &mut self,
        entries: Vec<records_host::records::Entry>,
    ) -> std::result::Result<records_host::records::ApplyResult, records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(records_host::records::Error::Unsupported)?
            .apply(entries)
    }
}

impl soil::http::http::Host for WebGpuHost {
    /// The guest adapter over `soil-host`'s one policy implementation.
    ///
    /// The guest names a URL and an opaque credential handle; the policy owns
    /// the origin allowlist, method/header/body checks and the transport. A
    /// host with no HTTP grant returns `unsupported`, not a silent empty
    /// response.
    fn fetch(
        &mut self,
        request: soil::http::http::Request,
    ) -> std::result::Result<soil::http::http::Response, soil::http::http::Error> {
        let Some(access) = self.http.as_ref() else {
            return Err(soil::http::http::Error::Unsupported);
        };
        let request = HttpRequest {
            url: request.url,
            method: match request.method {
                soil::http::http::Method::Get => http_host::Method::Get,
                soil::http::http::Method::Head => http_host::Method::Head,
                soil::http::http::Method::Post => http_host::Method::Post,
                soil::http::http::Method::Put => http_host::Method::Put,
                soil::http::http::Method::Patch => http_host::Method::Patch,
                soil::http::http::Method::Delete => http_host::Method::Delete,
            },
            headers: request
                .headers
                .into_iter()
                .map(|header| (header.name, header.value))
                .collect(),
            body: request.body,
            credential: request.credential,
            max_bytes: request.max_bytes,
            timeout_ms: request.timeout_ms,
        };
        let response = http_host::perform(
            &access.policy,
            &*access.credentials,
            &*access.transport,
            request,
        )
        .map_err(http_error)?;
        Ok(soil::http::http::Response {
            status: response.status,
            final_url: response.final_url,
            headers: response
                .headers
                .into_iter()
                .map(|(name, value)| soil::http::http::Header { name, value })
                .collect(),
            body: response.body,
        })
    }
}

fn http_error(error: http_host::Error) -> soil::http::http::Error {
    match error {
        http_host::Error::Denied => soil::http::http::Error::Denied,
        http_host::Error::Invalid => soil::http::http::Error::Invalid,
        http_host::Error::OverBudget => soil::http::http::Error::OverBudget,
        http_host::Error::Timeout => soil::http::http::Error::Timeout,
        http_host::Error::Unavailable => soil::http::http::Error::Unavailable,
        http_host::Error::Tls => soil::http::http::Error::Tls,
        http_host::Error::Http => soil::http::http::Error::Http,
        http_host::Error::Io => soil::http::http::Error::Io,
        http_host::Error::Unsupported => soil::http::http::Error::Unsupported,
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
                mark: entry.mark,
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

/// One per `observe.subscribe`.
pub struct AgentSubscription;

fn agent_error(error: soil_agent_host::Error) -> soil::agents::types::Error {
    use crate::soil::agents::types::Error;
    match error {
        soil_agent_host::Error::Denied => Error::Denied,
        soil_agent_host::Error::NotFound => Error::NotFound,
        soil_agent_host::Error::NotReady => Error::NotReady,
        soil_agent_host::Error::Invalid => Error::Invalid,
        soil_agent_host::Error::OverBudget => Error::OverBudget,
        soil_agent_host::Error::Stale => Error::Stale,
        soil_agent_host::Error::Unsupported => Error::Unsupported,
        soil_agent_host::Error::Io => Error::Io,
    }
}

fn agent_phase(phase: soil_agent_host::Phase) -> soil::agents::types::Phase {
    use crate::soil::agents::types::Phase;
    match phase {
        soil_agent_host::Phase::Connecting => Phase::Connecting,
        soil_agent_host::Phase::Idle => Phase::Idle,
        soil_agent_host::Phase::Thinking => Phase::Thinking,
        soil_agent_host::Phase::Working => Phase::Working,
        soil_agent_host::Phase::NeedsYou => Phase::NeedsYou,
        soil_agent_host::Phase::Finished => Phase::Finished,
        soil_agent_host::Phase::Failed => Phase::Failed,
        soil_agent_host::Phase::Paused => Phase::Paused,
    }
}

fn agent_activity_kind(kind: soil_agent_host::ActivityKind) -> soil::agents::types::ActivityKind {
    use crate::soil::agents::types::ActivityKind;
    match kind {
        soil_agent_host::ActivityKind::You => ActivityKind::You,
        soil_agent_host::ActivityKind::Model => ActivityKind::Model,
        soil_agent_host::ActivityKind::Memory => ActivityKind::Memory,
        soil_agent_host::ActivityKind::Files => ActivityKind::Files,
        soil_agent_host::ActivityKind::Shell => ActivityKind::Shell,
        soil_agent_host::ActivityKind::Network => ActivityKind::Network,
        soil_agent_host::ActivityKind::Browser => ActivityKind::Browser,
        soil_agent_host::ActivityKind::Agent => ActivityKind::Agent,
        soil_agent_host::ActivityKind::Skill => ActivityKind::Skill,
        soil_agent_host::ActivityKind::Hook => ActivityKind::Hook,
        soil_agent_host::ActivityKind::Other => ActivityKind::Other,
    }
}

fn agent_status(status: soil_agent_host::Status) -> soil::agents::types::Status {
    use crate::soil::agents::types::Status;
    match status {
        soil_agent_host::Status::Running => Status::Running,
        soil_agent_host::Status::Ok => Status::Ok,
        soil_agent_host::Status::Failed => Status::Failed,
        soil_agent_host::Status::Blocked => Status::Blocked,
    }
}

fn agent_prompt_kind(kind: soil_agent_host::PromptKind) -> soil::agents::types::PromptKind {
    use crate::soil::agents::types::PromptKind;
    match kind {
        soil_agent_host::PromptKind::Question => PromptKind::Question,
        soil_agent_host::PromptKind::Approval => PromptKind::Approval,
        soil_agent_host::PromptKind::Choice => PromptKind::Choice,
    }
}

fn agent_role(role: soil_agent_host::Role) -> soil::agents::types::Role {
    use crate::soil::agents::types::Role;
    match role {
        soil_agent_host::Role::User => Role::User,
        soil_agent_host::Role::Assistant => Role::Assistant,
        soil_agent_host::Role::Tool => Role::Tool,
    }
}

fn agent_owner(owner: soil_agent_host::Owner) -> soil::agents::types::Owner {
    soil::agents::types::Owner {
        actor: owner.actor,
        display_name: owner.display_name,
    }
}

fn agent_scope(scope: soil_agent_host::Scope) -> soil::agents::types::Scope {
    use crate::soil::agents::types::ScopeKind;
    soil::agents::types::Scope {
        kind: match scope.kind {
            soil_agent_host::ScopeKind::Device => ScopeKind::Device,
            soil_agent_host::ScopeKind::Desktop => ScopeKind::Desktop,
            soil_agent_host::ScopeKind::Principal => ScopeKind::Principal,
        },
        id: scope.id,
        label: scope.label,
        owner: scope.owner.map(agent_owner),
    }
}

fn agent_record(agent: soil_agent_host::Agent) -> soil::agents::types::Agent {
    soil::agents::types::Agent {
        id: agent.id,
        harness: agent.harness,
        title: agent.title,
        workspace: agent.workspace,
        model: agent.model,
        phase: agent_phase(agent.phase),
        goal: agent.goal.map(|goal| soil::agents::types::Goal {
            text: goal.text,
            set_at_ms: goal.set_at_ms,
        }),
        owner: agent.owner.map(agent_owner),
        activity: agent
            .activity
            .into_iter()
            .map(|row| soil::agents::types::Activity {
                kind: agent_activity_kind(row.kind),
                label: row.label,
                detail: row.detail,
                status: agent_status(row.status),
                started_at_ms: row.started_at_ms,
                finished_at_ms: row.finished_at_ms,
            })
            .collect(),
        prompts: agent
            .prompts
            .into_iter()
            .map(|prompt| soil::agents::types::Prompt {
                id: prompt.id,
                kind: agent_prompt_kind(prompt.kind),
                title: prompt.title,
                body: prompt.body,
                details: prompt.details,
                origin: prompt.origin,
                options: prompt
                    .choices
                    .into_iter()
                    .map(|choice| soil::agents::types::Choice {
                        id: choice.id,
                        label: choice.label,
                        detail: choice.detail,
                        danger: choice.danger,
                    })
                    .collect(),
                allows_text: prompt.allows_text,
                asked_at_ms: prompt.asked_at_ms,
                deadline_at_ms: prompt.deadline_at_ms,
            })
            .collect(),
        usage: soil::agents::types::Usage {
            context_tokens: agent.usage.context_tokens,
            context_window: agent.usage.context_window,
            input_tokens: agent.usage.input_tokens,
            output_tokens: agent.usage.output_tokens,
            turns: agent.usage.turns,
        },
        revision: agent.revision,
        started_at_ms: agent.started_at_ms,
        updated_at_ms: agent.updated_at_ms,
    }
}

fn agent_binding(binding: Binding) -> soil::agents::types::Binding {
    soil::agents::types::Binding {
        agent: binding.agent,
        view: binding.view,
    }
}

fn agent_page(page: Page) -> soil::agents::observe::Page {
    soil::agents::observe::Page {
        turns: page
            .turns
            .into_iter()
            .map(|turn| soil::agents::types::Turn {
                role: agent_role(turn.role),
                text: turn.text,
                at_ms: turn.at_ms,
            })
            .collect(),
        before: page.before,
    }
}

impl soil::agents::observe::Host for WebGpuHost {
    fn agents(
        &mut self,
    ) -> std::result::Result<Vec<soil::agents::types::Agent>, soil::agents::types::Error> {
        Ok(self.agents.list().into_iter().map(agent_record).collect())
    }

    fn get(
        &mut self,
        id: String,
    ) -> std::result::Result<soil::agents::types::Agent, soil::agents::types::Error> {
        self.agents
            .get(&id)
            .map(agent_record)
            .ok_or(soil::agents::types::Error::NotFound)
    }

    fn intent(&mut self) -> soil::agents::types::Binding {
        let binding = self.agents.binding();
        agent_binding(binding)
    }

    fn current_scope(&mut self) -> soil::agents::types::Scope {
        let scope = self.agents.scope();
        agent_scope(scope)
    }

    fn transcript(
        &mut self,
        id: String,
        before: Option<u64>,
        limit: u32,
    ) -> std::result::Result<soil::agents::observe::Page, soil::agents::types::Error> {
        self.agents
            .transcript(&id, before, limit)
            .map(agent_page)
            .map_err(agent_error)
    }

    fn subscribe(
        &mut self,
    ) -> std::result::Result<
        wasmtime::component::Resource<AgentSubscription>,
        soil::agents::types::Error,
    > {
        self.table
            .push(AgentSubscription)
            .map_err(|_| soil::agents::types::Error::Io)
    }
}

impl soil::agents::observe::HostSubscription for WebGpuHost {
    fn next(
        &mut self,
        subscription: wasmtime::component::Resource<AgentSubscription>,
        timeout_ms: u32,
    ) -> std::result::Result<Option<soil::agents::observe::Change>, soil::agents::types::Error>
    {
        // A handle the guest already dropped is `stale`, not an empty poll.
        self.table
            .get(&subscription)
            .map_err(|_| soil::agents::types::Error::Stale)?;
        Ok(self.agents.next(timeout_ms).map(|change| match change {
            soil_agent_host::Change::Upserted(agent) => {
                soil::agents::observe::Change::Upserted(agent_record(*agent))
            }
            soil_agent_host::Change::Removed(id) => soil::agents::observe::Change::Removed(id),
        }))
    }

    fn drop(
        &mut self,
        subscription: wasmtime::component::Resource<AgentSubscription>,
    ) -> std::result::Result<(), wasmtime::Error> {
        let _ = self.table.delete(subscription);
        Ok(())
    }
}

impl soil::agents::control::Host for WebGpuHost {
    fn respond(
        &mut self,
        id: String,
        prompt_id: String,
        answer: soil::agents::types::Response,
    ) -> std::result::Result<(), soil::agents::types::Error> {
        self.agents
            .answer(
                &id,
                &prompt_id,
                Answer {
                    choice: answer.choice,
                    text: answer.text,
                },
            )
            .map_err(agent_error)
    }

    fn send(
        &mut self,
        id: String,
        text: String,
    ) -> std::result::Result<(), soil::agents::types::Error> {
        self.agents.send(&id, &text).map_err(agent_error)
    }

    fn interrupt(&mut self, id: String) -> std::result::Result<(), soil::agents::types::Error> {
        self.agents.interrupt(&id).map_err(agent_error)
    }
}

impl soil::terminal_screen::screen::Host for WebGpuHost {
    fn sessions(
        &mut self,
    ) -> std::result::Result<Vec<soil::terminal_screen::screen::Session>, String> {
        Ok(self
            .terminal
            .sessions()?
            .into_iter()
            .map(|session| soil::terminal_screen::screen::Session {
                id: session.id,
                title: session.title,
                active: session.active,
            })
            .collect())
    }

    fn open_session(&mut self, command: Option<String>) -> std::result::Result<u32, String> {
        self.terminal.open_session(command)
    }

    fn select_session(&mut self, id: u32) -> std::result::Result<(), String> {
        self.terminal.select_session(id)
    }

    fn close_session(&mut self, id: u32) -> std::result::Result<(), String> {
        self.terminal.close_session(id)
    }

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

    fn wait(&mut self, after: u64, timeout_ms: u32) -> std::result::Result<u64, String> {
        self.terminal.wait(after, timeout_ms)
    }

    fn resize(&mut self, cols: u32, rows: u32) -> std::result::Result<(), String> {
        self.terminal.resize(cols, rows)
    }
}

impl soil::present::clipboard::Host for WebGpuHost {
    fn set_text(&mut self, text: String) -> std::result::Result<(), String> {
        match self.clipboard.as_mut() {
            Some(clipboard) => clipboard.set_text(text),
            None => Err("this host has no clipboard".to_owned()),
        }
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
    /// Where compiled components are cached. Compiling a Spore takes tens of
    /// seconds; deserializing one that has not changed takes milliseconds.
    component_cache: Option<std::path::PathBuf>,
    /// The default records grant, when the host was built around a records
    /// root. A caller can override it per run with `HostSetup::records`.
    records: Option<RecordsAccess>,
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
        soil::present::clipboard::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:present clipboard")?;
        soil::http::http::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:http")?;
        soil::spore::catalog::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:spore catalog")?;
        soil::agents::observe::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:agent observe")?;
        soil::agents::control::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:agent control")?;
        soil::terminal_screen::screen::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:terminal-screen")?;
        records_host::add_to_linker::<WebGpuHost>(&mut linker).context("link soil:records")?;
        ::soil::vfs_host::vfs::add_to_linker::<WebGpuHost, HasSelf<WebGpuHost>>(
            &mut linker,
            |host| host,
        )
        .map_err(anyhow::Error::from)
        .context("link soil:vfs")?;
        ::soil::principal_host::add_to_linker::<WebGpuHost>(&mut linker)
            .context("link soil:principal")?;
        Ok(Self {
            engine,
            linker,
            component_cache: None,
            records: None,
        })
    }

    /// Build a runtime whose guests may read the vitals view from this root.
    pub fn with_records(mut self, root: impl Into<PathBuf>) -> Self {
        self.records = Some(RecordsAccess::vitals(root));
        self
    }

    /// Keep compiled components in `directory`, named by the module's digest.
    /// A Spore that has not changed starts from the cache instead of being
    /// compiled again.
    pub fn with_component_cache(mut self, directory: impl Into<std::path::PathBuf>) -> Self {
        self.component_cache = Some(directory.into());
        self
    }

    /// Compile a component into the cache without running it. A Spore someone
    /// will open later costs nothing to start if it was paid for earlier.
    pub fn prewarm(&self, bytes: &[u8]) -> Result<()> {
        self.component(bytes).map(|_| ())
    }

    /// Compile a component, or reuse the cached compilation of the same bytes.
    fn component(&self, bytes: &[u8]) -> Result<Component> {
        let cache_path = self.component_cache.as_ref().map(|directory| {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(bytes);
            let mut name = String::with_capacity(digest.len() * 2 + 8);
            for byte in digest {
                use std::fmt::Write as _;
                let _ = write!(name, "{byte:02x}");
            }
            directory.join(format!("{name}.cwasm"))
        });
        if let Some(path) = &cache_path
            && let Ok(serialized) = std::fs::read(path)
        {
            // The file is ours: it is named by the digest of the module it was
            // compiled from, so a mismatch means a stale cache, not a bad actor.
            match unsafe { Component::deserialize(&self.engine, &serialized) } {
                Ok(component) => return Ok(component),
                Err(error) => {
                    eprintln!("soil-webgpu: cached component unusable ({error}); compiling");
                }
            }
        }
        let component = Component::new(&self.engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile WebGPU component")?;
        if let Some(path) = &cache_path {
            if let Some(directory) = path.parent() {
                let _ = std::fs::create_dir_all(directory);
            }
            match component.serialize() {
                Ok(serialized) => {
                    if let Err(error) = std::fs::write(path, serialized) {
                        eprintln!("soil-webgpu: could not cache component: {error}");
                    }
                }
                Err(error) => {
                    eprintln!("soil-webgpu: component cannot be serialized: {error}");
                }
            }
        }
        Ok(component)
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
    pub async fn run_component_with(&self, bytes: &[u8], mut setup: HostSetup) -> Result<u32> {
        if setup.records.is_none() {
            setup.records = self.records.clone();
        }
        let component = self.component(bytes)?;
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
    run_component_capture_with(bytes, width, height, None, None, None)
}

/// Capture one frame with a host that can answer `soil:spore/catalog` and
/// `soil:http` and `soil:agent`. A capture host is interactive=false, so the
/// guest renders one frame and exits: this is how a Spore's first screen is
/// inspected in CI.
pub fn run_component_capture_with(
    bytes: &[u8],
    width: u32,
    height: u32,
    catalog: Option<Box<dyn CatalogHost>>,
    http: Option<HttpAccess>,
    agents: Option<Box<dyn AgentBackend>>,
) -> Result<(u32, Option<CanvasFrame>)> {
    let runtime = WebGpuRuntime::new()?;
    let capture = FrameCapture::new(width, height);
    let mut setup = HostSetup::capture(Box::new(capture.clone()));
    if let Some(catalog) = catalog {
        setup.catalog = catalog;
    }
    if let Some(agents) = agents {
        setup.agents = agents;
    }
    setup.http = http;
    let status = pollster::block_on(runtime.run_component_with(bytes, setup))?;
    Ok((status, capture.take()))
}

/// What a capture run is allowed to reach.
///
/// Everything a Spore can be granted is named here, once, so a caller that
/// forgets a grant gets the denying default rather than a host that quietly
/// answers as somebody else.
#[derive(Default)]
pub struct Grants {
    /// One workspace the Spore may list and read through `soil:vfs`.
    pub vfs: Option<PathBuf>,
    /// The books the Spore may read or write through `soil:records`.
    pub records: Option<RecordsAccess>,
    /// Who the Spore is acting as, through `soil:principal`.
    pub principal: Option<PrincipalHost>,
    pub http: Option<HttpAccess>,
    pub catalog: Option<Box<dyn CatalogHost>>,
}

impl Grants {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attest `actor` as a human principal. Local development only: a host
    /// that verifies a real identity builds its own provider.
    pub fn actor(self, actor: impl Into<String>) -> Self {
        let principal = PrincipalHost::new(::soil::principal_host::StaticPrincipalProvider::new(
            ::soil::principal_host::Principal {
                actor: actor.into(),
                kind: ::soil::principal_host::PrincipalKind::Human,
                display_name: None,
                device: Some(gethostname()),
                teams: Vec::new(),
                roles: Vec::new(),
                generation: 1,
            },
        ));
        Self {
            principal: Some(principal),
            ..self
        }
    }
}

fn gethostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "localhost".into())
}

/// Capture one frame with whatever [`Grants`] the caller assembled.
pub fn run_component_capture_with_grants(
    bytes: &[u8],
    width: u32,
    height: u32,
    grants: Grants,
) -> Result<(u32, Option<CanvasFrame>)> {
    let runtime = WebGpuRuntime::new()?;
    let capture = FrameCapture::new(width, height);
    let mut setup = HostSetup::capture(Box::new(capture.clone()));
    setup.vfs = grants.vfs;
    setup.records = grants.records;
    setup.principal = grants.principal;
    setup.http = grants.http;
    if let Some(catalog) = grants.catalog {
        setup.catalog = catalog;
    }
    let status = pollster::block_on(runtime.run_component_with(bytes, setup))?;
    Ok((status, capture.take()))
}

/// Capture one frame with a read-only `vitals.view` records grant.
pub fn run_component_capture_with_records(
    bytes: &[u8],
    width: u32,
    height: u32,
    records_root: impl Into<PathBuf>,
) -> Result<(u32, Option<CanvasFrame>)> {
    let runtime = WebGpuRuntime::new()?.with_records(records_root);
    let capture = FrameCapture::new(width, height);
    let setup = HostSetup::capture(Box::new(capture.clone()));
    let status = pollster::block_on(runtime.run_component_with(bytes, setup))?;
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

    #[test]
    fn the_runner_offers_agents_but_not_their_authority() {
        // The point of the surface is that it can watch and answer. Spawning a
        // harness, choosing a model, or reading a credential is not here, and
        // dropping this assertion is how one of them would quietly arrive.
        assert!(RUNNER_WIT.contains("import soil:agents/observe@0.1.0;"));
        assert!(RUNNER_WIT.contains("import soil:agents/control@0.1.0;"));
        for forbidden in ["spawn", "config", "credential", "secrets"] {
            assert!(
                !RUNNER_WIT.contains(forbidden),
                "the agent surface grew `{forbidden}` authority"
            );
        }
    }
}
