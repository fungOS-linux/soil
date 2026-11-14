//! Bounded core-Wasm runner. Host authority is denied by default; explicit
//! capability grants expose only narrow typed operations, never WASI/devices.
use anyhow::{Context, Result, bail};
use fpl_gfx::{Primitive, PrimitiveKind, Rect, Scene, SoftwareImage, Theme, ThemeRole};
use wasmtime::{Config, Engine, Module, ResourceLimiter, Store, StoreLimits, StoreLimitsBuilder};

/// Convert an internal anyhow failure into the error type Wasmtime host
/// functions must return. Wasmtime 50 no longer implements `std::error::Error`
/// for its own error, so `?`/`From` cannot bridge the two automatically.
pub(crate) fn host<T>(result: Result<T>) -> wasmtime::Result<T> {
    result.map_err(wasmtime::Error::from_anyhow)
}

pub mod battery;
pub mod canvas;
pub mod estimate;
pub mod evdev;
pub mod keypad;
pub mod lid;

pub const PHONE_SHELL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/phone-shell.wasm"));

pub const MAX_MODULE_BYTES: usize = 2 * 1024 * 1024;
/// Per-Spore linear-memory ceiling.
///
/// A UI Spore needs room for fonts, its own rasterizer and two 480x640 RGBA
/// frames (~2.4 MiB). 256 MiB is generous headroom without letting one Spore
/// starve the device it shares with the OS.
pub const MAX_MEMORY_BYTES: usize = 256 * 1024 * 1024;

/// Ceiling for every Spore in one host process. Spores share a phone with the
/// OS and each other, so the per-Spore number alone is not a policy.
pub const DEFAULT_TOTAL_MEMORY_BYTES: usize = 1536 * 1024 * 1024;

pub const DEFAULT_FUEL: u64 = 10_000_000;

/// Linear-memory allowances: one Spore ceiling and one shared total.
///
/// `Clone` is shallow on purpose: every clone charges the same counter, so
/// sharing a budget across spores is a `clone()`, and a fresh `default()` is a
/// fresh host process.
#[derive(Clone, Debug)]
pub struct MemoryBudget {
    per_spore: usize,
    total: usize,
    in_use: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Default for MemoryBudget {
    fn default() -> Self {
        Self::new(MAX_MEMORY_BYTES, DEFAULT_TOTAL_MEMORY_BYTES)
    }
}

impl MemoryBudget {
    pub fn new(per_spore: usize, total: usize) -> Self {
        Self {
            per_spore,
            total,
            in_use: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
    pub fn per_spore(&self) -> usize {
        self.per_spore
    }
    pub fn total(&self) -> usize {
        self.total
    }
    pub fn in_use(&self) -> usize {
        self.in_use.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Charge `bytes` against the shared total, all or nothing.
    fn charge(&self, bytes: usize) -> bool {
        self.in_use
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |used| {
                    let next = used.saturating_add(bytes);
                    (next <= self.total).then_some(next)
                },
            )
            .is_ok()
    }
    fn release(&self, bytes: usize) {
        if bytes != 0 {
            self.in_use
                .fetch_sub(bytes, std::sync::atomic::Ordering::AcqRel);
        }
    }
}

/// What one store has charged against its [`MemoryBudget`].
struct MemoryCharge {
    budget: MemoryBudget,
    charged: usize,
}

struct Limits(
    StoreLimits,
    lid::LidHost,
    canvas::CanvasHost,
    battery::BatteryHost,
    estimate::EstimateHost,
    MemoryCharge,
);
impl ResourceLimiter for Limits {
    /// The per-Spore ceiling comes from `StoreLimits`; the shared total is
    /// charged here, so one Spore cannot starve the others by growing into
    /// memory they are about to need.
    fn memory_growing(&mut self, c: usize, d: usize, m: Option<usize>) -> wasmtime::Result<bool> {
        if !self.0.memory_growing(c, d, m)? {
            return Ok(false);
        }
        let delta = d.saturating_sub(c);
        if !self.5.budget.charge(delta) {
            return Ok(false);
        }
        self.5.charged += delta;
        Ok(true)
    }
    fn table_growing(&mut self, c: usize, d: usize, m: Option<usize>) -> wasmtime::Result<bool> {
        self.0.table_growing(c, d, m)
    }
    fn instances(&self) -> usize {
        self.0.instances()
    }
    fn tables(&self) -> usize {
        self.0.tables()
    }
    fn memories(&self) -> usize {
        self.0.memories()
    }
}
impl Drop for Limits {
    fn drop(&mut self) {
        let charged = self.5.charged;
        self.5.budget.release(charged);
    }
}

pub const MAX_SCENE_BYTES: usize = fpl_gfx::software::MAX_PRIMITIVES * RECORD_SIZE;
pub const WIDTH: u32 = 480;
pub const HEIGHT: u32 = 640;

/// Normalized keys; device codes and services remain host-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Key {
    Up = 1,
    Down = 2,
    Left = 3,
    Right = 4,
    Select = 5,
    Back = 6,
    Home = 7,
}

impl Key {
    /// Reuse the extracted Canvas Viewer Spore owner contract. Up/down remain
    /// host chrome/focus keys, not invented content-navigation actions.
    pub fn viewer_key(self) -> viewer_spore::RemoteKey {
        use viewer_spore::RemoteKey;
        match self {
            Self::Up => RemoteKey::DpadUp,
            Self::Down => RemoteKey::DpadDown,
            Self::Left => RemoteKey::DpadLeft,
            Self::Right => RemoteKey::DpadRight,
            Self::Select => RemoteKey::Ok,
            Self::Back | Self::Home => RemoteKey::Back,
        }
    }
}

/// One persistent, import-free instance. Every invocation receives fresh fuel.
pub struct GuestScene {
    store: Store<Limits>,
    instance: wasmtime::Instance,
    fuel: u64,
}

fn validate_module(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_MODULE_BYTES {
        bail!("module_too_large");
    }
    if !bytes.starts_with(b"\0asm") {
        bail!("bad_wasm_magic");
    }
    Ok(())
}

/// One guest-authored primitive record in exported memory, little-endian.
///
/// Layout (32 bytes): `x f32, y f32, w f32, h f32`, then `kind u8`, `fill u8`,
/// `outline u8`, `accent u8`, then `radius f32, value f32, rotation f32`.
///
/// The host validates every field before handing it to the shared software
/// rasterizer, so a guest can never smuggle unsupported semantics through.
#[derive(Clone, Copy)]
struct GuestRecord {
    rect: [f32; 4],
    kind: u8,
    fill: u8,
    outline: u8,
    accent: u8,
    radius: f32,
    value: f32,
    rotation: f32,
}

const RECORD_SIZE: usize = 32;
const KIND_ROUNDED_RECT: u8 = 0;
const KIND_DISC: u8 = 1;

impl GuestRecord {
    fn from_le(bytes: &[u8]) -> Self {
        let f = |i: usize| f32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        Self {
            rect: [f(0), f(4), f(8), f(12)],
            kind: bytes[16],
            fill: bytes[17],
            outline: bytes[18],
            accent: bytes[19],
            radius: f(20),
            value: f(24),
            rotation: f(28),
        }
    }
}

/// Render a bounded, host-validated guest scene with the shared `fpl-gfx`
/// software rasterizer (commit `82b14ff`). The guest stays import-free; it only
/// publishes a `run: () -> i32` offset plus a `scene_len: i32` byte length.
///
/// All bounds, primitive kinds, and theme roles are constrained here and
/// rejected with a descriptive error when invalid, so the rasterizer never
/// sees unsupported semantics.
pub fn render_guest_scene(bytes: &[u8], fuel: u64) -> Result<SoftwareImage> {
    GuestScene::new(bytes, fuel)?.render()
}

impl GuestScene {
    pub fn new(bytes: &[u8], fuel: u64) -> Result<Self> {
        Self::with_lid(bytes, fuel, false, None)
    }

    /// Explicitly enables the lid interface, with a separate per-guest permission.
    /// `None` exposes typed Denied, without opening any provider connection.
    pub fn with_lid(
        bytes: &[u8],
        fuel: u64,
        enable: bool,
        grant: Option<lid::LidGrant>,
    ) -> Result<Self> {
        Self::instantiate(
            bytes,
            fuel,
            enable,
            grant,
            false,
            None,
            None,
            None,
            false,
            MemoryBudget::default(),
        )
    }

    /// Grants only a host-selected actual Canvas owner surface, never raw DRM.
    pub fn with_canvas(
        bytes: &[u8],
        fuel: u64,
        surface: Option<Box<dyn canvas::CanvasSurface>>,
    ) -> Result<Self> {
        Self::instantiate(
            bytes,
            fuel,
            false,
            None,
            true,
            surface,
            None,
            None,
            false,
            MemoryBudget::default(),
        )
    }

    /// Same grant as [`Self::with_canvas`], with a shared memory budget so a
    /// host can cap every Spore it runs together.
    pub fn with_canvas_budget(
        bytes: &[u8],
        fuel: u64,
        surface: Option<Box<dyn canvas::CanvasSurface>>,
        budget: MemoryBudget,
    ) -> Result<Self> {
        Self::instantiate(bytes, fuel, false, None, true, surface, None, None, false, budget)
    }

    pub fn with_battery_canvas(
        bytes: &[u8],
        fuel: u64,
        surface: Option<Box<dyn canvas::CanvasSurface>>,
        grant: Option<battery::BatteryGrant>,
    ) -> Result<Self> {
        Self::instantiate(
            bytes,
            fuel,
            false,
            None,
            true,
            surface,
            Some(grant),
            None,
            false,
            MemoryBudget::default(),
        )
    }

    /// Separate estimate authority; no implicit battery grant.
    pub fn with_estimate_canvas(
        bytes: &[u8],
        fuel: u64,
        surface: Option<Box<dyn canvas::CanvasSurface>>,
        grant: Option<estimate::EstimateGrant>,
    ) -> Result<Self> {
        Self::instantiate(
            bytes,
            fuel,
            false,
            None,
            true,
            surface,
            None,
            Some(grant),
            false,
            MemoryBudget::default(),
        )
    }

    /// Explicit animation-only clock grant in addition to the battery capability.
    pub fn with_animated_battery_canvas(
        bytes: &[u8],
        fuel: u64,
        surface: Option<Box<dyn canvas::CanvasSurface>>,
        grant: Option<battery::BatteryGrant>,
    ) -> Result<Self> {
        Self::instantiate(
            bytes,
            fuel,
            false,
            None,
            true,
            surface,
            Some(grant),
            None,
            true,
            MemoryBudget::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn instantiate(
        bytes: &[u8],
        fuel: u64,
        enable: bool,
        grant: Option<lid::LidGrant>,
        enable_canvas: bool,
        surface: Option<Box<dyn canvas::CanvasSurface>>,
        battery: Option<Option<battery::BatteryGrant>>,
        estimate: Option<Option<estimate::EstimateGrant>>,
        animation: bool,
        budget: MemoryBudget,
    ) -> Result<Self> {
        let enable_battery = battery.is_some();
        let enable_estimate = estimate.is_some();
        validate_module(bytes)?;
        let mut config = Config::new();
        config.consume_fuel(true);
        config.cranelift_nan_canonicalization(true);
        let engine = Engine::new(&config)?;
        let module = Module::new(&engine, bytes)?;
        if !enable && !enable_canvas && module.imports().count() != 0 {
            bail!("host_imports");
        }
        let limits = StoreLimitsBuilder::new()
            .memory_size(MAX_MEMORY_BYTES)
            .table_elements(4096)
            .tables(1)
            .memories(1)
            .instances(1)
            .build();
        let mut store = Store::new(
            &engine,
            Limits(
                limits,
                lid::LidHost::new(grant),
                canvas::CanvasHost {
                    surface,
                    ..Default::default()
                },
                battery::BatteryHost::new(enable_battery, battery.flatten()),
                estimate::EstimateHost::new(enable_estimate, estimate.flatten()),
                MemoryCharge {
                    budget,
                    charged: 0,
                },
            ),
        );
        store.limiter(|x| x as &mut dyn ResourceLimiter);
        store.set_fuel(fuel)?;
        let mut linker = wasmtime::Linker::new(&engine);
        if enable {
            lid::link(&mut linker)?;
        }
        if enable_canvas {
            canvas::link(&mut linker)?;
        }
        if enable_battery {
            battery::link(&mut linker)?;
        }
        if enable_estimate {
            estimate::link(&mut linker)?;
        }
        if animation {
            canvas::link_animation(&mut linker)?;
        }
        let instance = linker.instantiate(&mut store, &module)?;
        Ok(Self {
            store,
            instance,
            fuel,
        })
    }

    /// Execute the Canvas guest's request and return ONLY the owner-rendered
    /// frame retained by the host import, not guest-authored primitive memory.
    pub fn render_canvas(&mut self) -> Result<SoftwareImage> {
        self.store.data_mut().2.reset();
        self.store.data_mut().3.reset();
        self.store.data_mut().4.reset();
        self.store.set_fuel(self.fuel)?;
        let run = self
            .instance
            .get_typed_func::<(), i32>(&mut self.store, "run")?;
        let status = run.call(&mut self.store, ())?;
        anyhow::ensure!(status == 0, "canvas capability status {status}");
        self.store
            .data_mut()
            .2
            .frame
            .take()
            .context("Canvas guest did not present owner frame")
    }

    /// Time authority is host-selected; guest may only relay the exact token time.
    pub fn tick(&mut self, elapsed_ms: u64) -> Result<()> {
        anyhow::ensure!(elapsed_ms <= i64::MAX as u64, "frame time out of range");
        self.store.data_mut().2.reset();
        self.store.data_mut().2.expected_time = Some(elapsed_ms);
        self.store.set_fuel(self.fuel)?;
        let tick = self
            .instance
            .get_typed_func::<i64, ()>(&mut self.store, "tick")?;
        let result = tick.call(&mut self.store, elapsed_ms as i64);
        let consumed = self.store.data_mut().2.expected_time.take().is_none();
        result?;
        anyhow::ensure!(consumed, "guest did not relay frame time");
        Ok(())
    }

    pub fn take_lid_logs(&mut self) -> Vec<lid::Observation> {
        std::mem::take(&mut self.store.data_mut().1.logs)
    }

    pub fn key(&mut self, key: Key) -> Result<()> {
        self.store.data_mut().2.reset();
        self.store.data_mut().3.reset();
        self.store.data_mut().4.reset();
        self.store.data_mut().1.reset();
        self.store.set_fuel(self.fuel)?;
        let input = self
            .instance
            .get_typed_func::<i32, ()>(&mut self.store, "input")
            .map_err(anyhow::Error::from)
            .context("required export input: (i32) -> ()")?;
        input
            .call(&mut self.store, key as i32)
            .map_err(anyhow::Error::from)
            .context("guest input")
    }

    pub fn render(&mut self) -> Result<SoftwareImage> {
        self.store.data_mut().1.reset();
        self.store.set_fuel(self.fuel)?;
        let store = &mut self.store;
        let instance = &self.instance;
        let run = instance.get_typed_func::<(), i32>(&mut *store, "run")?;
        let ptr = usize::try_from(run.call(&mut *store, ()).map_err(anyhow::Error::from)?)
            .context("negative scene offset")?;
        let len = usize::try_from(
            instance
                .get_global(&mut *store, "scene_len")
                .context("scene_len global")?
                .get(&mut *store)
                .i32()
                .context("scene_len type")?,
        )
        .context("negative scene length")?;
        if len > MAX_SCENE_BYTES || len % RECORD_SIZE != 0 {
            bail!("malformed_scene_length");
        }
        let memory = instance
            .get_memory(&mut *store, "memory")
            .context("memory export")?;
        let end = ptr.checked_add(len).context("scene bounds overflow")?;
        let records = memory
            .data(&*store)
            .get(ptr..end)
            .context("scene outside guest memory")?;

        let mut scene = Scene::new();
        let theme = Theme::studio_dark();
        for chunk in records.as_chunks::<RECORD_SIZE>().0 {
            let g = GuestRecord::from_le(chunk);
            let [x, y, w, h] = g.rect;
            if ![x, y, w, h].iter().all(|v| v.is_finite())
                || x < 0.0
                || y < 0.0
                || w <= 0.0
                || h <= 0.0
                || x + w > fpl_gfx::software::MAX_WIDTH as f32
                || y + h > fpl_gfx::software::MAX_HEIGHT as f32
            {
                bail!("invalid_scene_bounds");
            }
            if !g.radius.is_finite()
                || g.radius < 0.0
                || !g.value.is_finite()
                || !g.rotation.is_finite()
                || g.value != 0.0
                || g.rotation != 0.0
                || (g.kind == KIND_DISC && (g.radius != 0.0 || g.accent != g.fill))
            {
                bail!("invalid_primitive_field");
            }
            let kind = match g.kind {
                KIND_ROUNDED_RECT => PrimitiveKind::RoundedRect,
                KIND_DISC => PrimitiveKind::Disc,
                _ => bail!("unsupported_primitive_kind"),
            };
            let fill = role(g.fill)?;
            let accent = role(g.accent)?;
            let outline = role(g.outline)?;

            // The shared rasterizer accepts only solid discs and flat, un-outlined
            // rounded rectangles. Enforce that contract here so the guest cannot
            // emit an unsupported style that would otherwise error downstream.
            let primitive = match kind {
                PrimitiveKind::Disc => {
                    if outline != ThemeRole::Outline {
                        bail!("disc outline must be Outline role");
                    }
                    Primitive::disc(Rect::new(x, y, w, h), fill)
                }
                PrimitiveKind::RoundedRect => {
                    if outline != ThemeRole::Outline {
                        bail!("rounded rect outline must be Outline role");
                    }
                    if g.value != 0.0 || g.rotation != 0.0 || g.radius > w.min(h) / 2.0 {
                        bail!("rounded rect style out of range");
                    }
                    Primitive {
                        rect: Rect::new(x, y, w, h),
                        kind,
                        fill,
                        outline,
                        accent,
                        radius: g.radius,
                        outline_width: 0.0,
                        value: 0.0,
                        rotation: 0.0,
                        gloss: 0.0,
                        grain: 0.0,
                        emission: 0.0,
                    }
                }
                _ => bail!("unsupported_primitive_kind"),
            };
            scene.push(primitive);
        }
        Ok(fpl_gfx::software::render(&scene, &theme, WIDTH, HEIGHT)?)
    }
}

/// Map a u8 byte to a ThemeRole, rejecting values outside the defined schema.
fn role(byte: u8) -> Result<ThemeRole> {
    Ok(match byte {
        0 => ThemeRole::Background,
        1 => ThemeRole::Surface,
        2 => ThemeRole::SurfaceRaised,
        3 => ThemeRole::SurfaceRecessed,
        4 => ThemeRole::Text,
        5 => ThemeRole::TextMuted,
        6 => ThemeRole::Primary,
        7 => ThemeRole::Secondary,
        8 => ThemeRole::Success,
        9 => ThemeRole::Warning,
        10 => ThemeRole::Danger,
        11 => ThemeRole::MeterLow,
        12 => ThemeRole::MeterHigh,
        13 => ThemeRole::Outline,
        14 => ThemeRole::Shadow,
        15 => ThemeRole::Highlight,
        _ => bail!("invalid theme role"),
    })
}

/// Invoke an exported `run: () -> i32` function under strict, explicit limits.
pub fn run(bytes: &[u8], fuel: u64) -> Result<i32> {
    if bytes.len() > MAX_MODULE_BYTES {
        bail!("module_too_large: {} > {}", bytes.len(), MAX_MODULE_BYTES);
    }
    if !bytes.starts_with(b"\0asm") {
        bail!("bad_wasm_magic");
    }
    let mut config = Config::new();
    config.consume_fuel(true);
    config.cranelift_nan_canonicalization(true);
    let engine = Engine::new(&config)
        .map_err(anyhow::Error::from)
        .context("create Wasmtime engine")?;
    let module = Module::new(&engine, bytes)
        .map_err(anyhow::Error::from)
        .context("compile core Wasm")?;
    let imports = module.imports().count();
    if imports != 0 {
        bail!("host_imports: module requests {imports} import(s)");
    }
    let limits = StoreLimitsBuilder::new()
        .memory_size(MAX_MEMORY_BYTES)
        .table_elements(4096)
        .tables(1)
        .memories(1)
        .instances(1)
        .build();
    let mut store = Store::new(
        &engine,
        Limits(
            limits,
            lid::LidHost::default(),
            canvas::CanvasHost::default(),
            battery::BatteryHost::default(),
            estimate::EstimateHost::default(),
            MemoryCharge {
                budget: MemoryBudget::default(),
                charged: 0,
            },
        ),
    );
    store.limiter(|x| x as &mut dyn ResourceLimiter);
    store
        .set_fuel(fuel)
        .map_err(anyhow::Error::from)
        .context("set fuel")?;
    let instance = wasmtime::Instance::new(&mut store, &module, &[])
        .map_err(anyhow::Error::from)
        .context("instantiate")?;
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .map_err(anyhow::Error::from)
        .context("required export run: () -> i32")?;
    run.call(&mut store, ())
        .map_err(anyhow::Error::from)
        .context("execute (fuel exhaustion is a hard failure)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_navigation_is_persistent_clamped_and_reversible() {
        let mut guest = GuestScene::new(PHONE_SHELL, DEFAULT_FUEL).unwrap();
        let initial = guest.render().unwrap();
        assert_eq!(initial, guest.render().unwrap());
        assert!(
            initial
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 255)
        );
        guest.key(Key::Up).unwrap();
        assert_eq!(initial, guest.render().unwrap());
        guest.key(Key::Down).unwrap();
        let next = guest.render().unwrap();
        assert_ne!(initial, next);
        guest.key(Key::Select).unwrap();
        assert_ne!(next, guest.render().unwrap());
        guest.key(Key::Back).unwrap();
        assert_eq!(next, guest.render().unwrap());
        guest.key(Key::Left).unwrap();
        assert_eq!(initial, guest.render().unwrap());
        for _ in 0..3 {
            guest.key(Key::Right).unwrap();
        }
        let last = guest.render().unwrap();
        guest.key(Key::Down).unwrap();
        assert_eq!(last, guest.render().unwrap());
        let bgr = last.to_bgr24().unwrap();
        assert_eq!(bgr.len(), (WIDTH * HEIGHT * 3) as usize);
        assert_eq!(&bgr[..3], &[last.pixels[2], last.pixels[1], last.pixels[0]]);
    }

    #[test]
    fn scene_limits_and_unsupported_fields_are_rejected() {
        assert!(
            render_guest_scene(&vec![0; MAX_MODULE_BYTES + 1], DEFAULT_FUEL)
                .unwrap_err()
                .to_string()
                .contains("module_too_large")
        );
        assert!(
            render_guest_scene(b"not wasm", DEFAULT_FUEL)
                .unwrap_err()
                .to_string()
                .contains("bad_wasm_magic")
        );
        for (ptr, len, expected) in [
            (-1, 32, "negative scene offset"),
            (65530, 32, "scene outside"),
            (0, -1, "negative scene length"),
            (0, MAX_SCENE_BYTES as i32 + 32, "malformed_scene_length"),
        ] {
            let wasm = wat::parse_str(format!("(module (memory (export \"memory\") 1) (global (export \"scene_len\") i32 (i32.const {len})) (func (export \"run\") (result i32) i32.const {ptr}))")).unwrap();
            assert!(
                render_guest_scene(&wasm, DEFAULT_FUEL)
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
        for (radius, value, rotation) in [(1., 0., 0.), (0., 0.5, 0.), (0., 0., 1.)] {
            let wasm = record_module(
                0., 0., 10., 10., KIND_DISC, 6, 13, 6, radius, value, rotation,
            );
            assert!(render_guest_scene(&wasm, DEFAULT_FUEL).is_err());
        }
        // One page past the ceiling, derived so a cap change cannot silently
        // turn this into a passing test of nothing.
        let pages = MAX_MEMORY_BYTES / 65_536 + 1;
        let wasm = wat::parse_str(format!(
            "(module (memory {pages}) (func (export \"run\") (result i32) i32.const 0))"
        ))
        .unwrap();
        assert!(GuestScene::new(&wasm, DEFAULT_FUEL).is_err());
    }

    #[test]
    fn memory_budget_is_shared_across_spores() {
        let budget = MemoryBudget::new(1024, 2048);
        let other_spore = budget.clone();
        assert!(budget.charge(1024));
        assert_eq!(budget.in_use(), 1024);
        // The second Spore may use what is left of the shared total, no more.
        assert!(!other_spore.charge(1025));
        assert!(other_spore.charge(1024));
        assert_eq!(budget.in_use(), 2048);
        other_spore.release(1024);
        assert_eq!(budget.in_use(), 1024);
        other_spore.release(1024);
        assert_eq!(budget.in_use(), 0);
    }

    #[test]
    fn a_default_budget_is_one_spore_ceiling_and_a_device_total() {
        let budget = MemoryBudget::default();
        assert_eq!(budget.per_spore(), MAX_MEMORY_BYTES);
        assert_eq!(budget.total(), DEFAULT_TOTAL_MEMORY_BYTES);
        assert_eq!(budget.in_use(), 0);
    }

    /// Build a guest module that writes one 32-byte primitive record at offset 0,
    /// sets `scene_len` to 32, and returns 0.
    #[allow(clippy::too_many_arguments)] // Mirrors the wire-record fields in adversarial fixtures.
    fn record_module(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        kind: u8,
        fill: u8,
        outline: u8,
        accent: u8,
        radius: f32,
        value: f32,
        rotation: f32,
    ) -> Vec<u8> {
        let s = format!(
            "(module (memory (export \"memory\") 1)\n\
             (global (export \"scene_len\") (mut i32) (i32.const 0))\n\
             (func (export \"run\") (result i32)\n\
             (f32.store (i32.const 0) (f32.const {x}))\n\
             (f32.store (i32.const 4) (f32.const {y}))\n\
             (f32.store (i32.const 8) (f32.const {w}))\n\
             (f32.store (i32.const 12) (f32.const {h}))\n\
             (i32.store8 (i32.const 16) (i32.const {kind}))\n\
             (i32.store8 (i32.const 17) (i32.const {fill}))\n\
             (i32.store8 (i32.const 18) (i32.const {outline}))\n\
             (i32.store8 (i32.const 19) (i32.const {accent}))\n\
             (f32.store (i32.const 20) (f32.const {radius}))\n\
             (f32.store (i32.const 24) (f32.const {value}))\n\
             (f32.store (i32.const 28) (f32.const {rotation}))\n\
             (global.set 0 (i32.const 32))\n\
             (i32.const 0)))",
        );
        wat::parse_str(&s).unwrap()
    }

    fn disc(x: f32, y: f32, w: f32, h: f32) -> Vec<u8> {
        record_module(x, y, w, h, KIND_DISC, 6, 13, 6, 0.0, 0.0, 0.0)
    }

    #[test]
    fn renders_guest_disc_offscreen() {
        // GFX samples pixel centers. All four samples of this 2x2 disc are inside.
        let image = render_guest_scene(&disc(0.0, 0.0, 2.0, 2.0), 1_000_000).unwrap();
        assert_eq!((image.width, image.height), (WIDTH, HEIGHT));
        assert_eq!(image.pixels.len(), (WIDTH * HEIGHT * 4) as usize);
        let fill = Theme::studio_dark().colors[ThemeRole::Primary as usize].to_srgb8();
        assert_eq!(&image.pixels[..4], &fill);
        let far = ((300 * WIDTH + 300) * 4) as usize;
        assert_eq!(&image.pixels[far..far + 4], &[0, 0, 0, 0]);
        assert_eq!(
            image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| **p == fill)
                .count(),
            4
        );
    }

    #[test]
    fn rejects_malformed_scene_length() {
        let s = "(module (memory (export \"memory\") 1)\n\
                 (global (export \"scene_len\") (mut i32) (i32.const 33))\n\
                 (func (export \"run\") (result i32) (global.set 0 (i32.const 33)) (i32.const 0)))";
        let wasm = wat::parse_str(s).unwrap();
        let err = render_guest_scene(&wasm, 1_000_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("malformed_scene_length"), "got: {err}");
    }

    #[test]
    fn rejects_invalid_bounds() {
        // Zero width.
        let wasm = disc(0.0, 0.0, 0.0, 10.0);
        assert!(
            render_guest_scene(&wasm, 1_000_000)
                .unwrap_err()
                .to_string()
                .contains("invalid_scene_bounds")
        );
        // Out of the 480x640 extent.
        let wasm = disc(0.0, 0.0, 481.0, 10.0);
        assert!(
            render_guest_scene(&wasm, 1_000_000)
                .unwrap_err()
                .to_string()
                .contains("invalid_scene_bounds")
        );
    }

    #[test]
    fn rejects_nan_bounds() {
        // NAN x: emit it via bit pattern 0x7fc00000 (quiet NaN).
        let s = "(module (memory (export \"memory\") 1)\n\
                 (global (export \"scene_len\") (mut i32) (i32.const 0))\n\
                 (func (export \"run\") (result i32)\n\
                 (f32.store (i32.const 0) (f32.const nan))\n\
                 (f32.store (i32.const 4) (f32.const 0.0))\n\
                 (f32.store (i32.const 8) (f32.const 10.0))\n\
                 (f32.store (i32.const 12) (f32.const 10.0))\n\
                 (i32.store8 (i32.const 16) (i32.const 1))\n\
                 (i32.store8 (i32.const 17) (i32.const 6))\n\
                 (i32.store8 (i32.const 18) (i32.const 13))\n\
                 (i32.store8 (i32.const 19) (i32.const 6))\n\
                 (f32.store (i32.const 20) (f32.const 0.0))\n\
                 (f32.store (i32.const 24) (f32.const 0.0))\n\
                 (f32.store (i32.const 28) (f32.const 0.0))\n\
                 (global.set 0 (i32.const 32))\n\
                 (i32.const 0)))";
        let wasm = wat::parse_str(s).unwrap();
        assert!(
            render_guest_scene(&wasm, 1_000_000)
                .unwrap_err()
                .to_string()
                .contains("invalid_scene_bounds")
        );
    }

    #[test]
    fn rejects_unsupported_primitive_kind() {
        let wasm = record_module(0.0, 0.0, 10.0, 10.0, 2, 6, 13, 6, 0.0, 0.0, 0.0);
        assert!(
            render_guest_scene(&wasm, 1_000_000)
                .unwrap_err()
                .to_string()
                .contains("unsupported_primitive_kind")
        );
    }

    #[test]
    fn rejects_theme_role_out_of_range() {
        let wasm = record_module(0.0, 0.0, 10.0, 10.0, KIND_DISC, 99, 13, 99, 0.0, 0.0, 0.0);
        assert!(
            render_guest_scene(&wasm, 1_000_000)
                .unwrap_err()
                .to_string()
                .contains("invalid theme role")
        );
    }

    #[test]
    fn rejects_rounded_rect_with_value() {
        // A rounded rect that tries to carry a value is unsupported by the rasterizer.
        let wasm = record_module(
            0.0,
            0.0,
            10.0,
            10.0,
            KIND_ROUNDED_RECT,
            6,
            13,
            6,
            2.0,
            0.5,
            0.0,
        );
        assert!(
            render_guest_scene(&wasm, 1_000_000)
                .unwrap_err()
                .to_string()
                .contains("invalid_primitive_field")
        );
    }

    #[test]
    fn rejects_fuel_exhaustion() {
        let wasm = wat::parse_str(
            "(module (memory (export \"memory\") 1)\n\
             (global (export \"scene_len\") (mut i32) (i32.const 0))\n\
             (func (export \"run\") (result i32) (block (loop br 0)) (i32.const 0)))",
        )
        .unwrap();
        let err = render_guest_scene(&wasm, 1).unwrap_err().to_string();
        assert!(
            err.contains("fuel") || err.to_lowercase().contains("exhaust") || err.contains("wasm"),
            "got: {err}"
        );
    }

    #[test]
    fn executes_import_free_fixture_deterministically() {
        let wasm =
            wat::parse_str("(module (func (export \"run\") (result i32) i32.const 42))").unwrap();
        assert_eq!(run(&wasm, 1000).unwrap(), 42);
        assert_eq!(run(&wasm, 1000).unwrap(), 42);
    }
    #[test]
    fn rejects_imports() {
        let wasm = wat::parse_str("(module (import \"env\" \"x\" (func)) (func (export \"run\") (result i32) i32.const 1))").unwrap();
        assert!(
            run(&wasm, 1000)
                .unwrap_err()
                .to_string()
                .contains("host_imports")
        );
    }
}
