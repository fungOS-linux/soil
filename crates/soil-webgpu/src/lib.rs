//! Bounded `wasi:webgpu` host for Soil.
//!
//! This is intentionally a separate crate from `soil-runtime`: the Canvas
//! runtime is a core-Wasm host, while WebGPU is a component-model capability.
//! A Spore that imports `wasi:webgpu` also gets the narrow Canvas presentation
//! bridge and, for the vitals renderer, a read-only `vitals.view` records
//! namespace. Adapter/device creation, validation, limits and queue submission
//! stay in the host through `wgpu-core`; no native handle crosses the boundary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use soil_records_host::{Namespace, NamespacedRecordsHost, records as records_host};
use wasi_webgpu_wasmtime::reexports::{wgpu_core, wgpu_types};
use wasi_webgpu_wasmtime::{WasiWebGpuCtx, WasiWebGpuCtxView, WasiWebGpuOptions};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

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

/// Host-side destination for a guest `soil:canvas-gpu` frame.
///
/// Canvas owns the actual window and final composition; this trait only lets
/// the WebGPU host hand over a completed frame. It is never called from inside
/// a guest callback.
pub trait CanvasGpuPresenter: Send {
    fn size(&self) -> (u32, u32);
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
        anyhow::ensure!(slot.is_none(), "canvas present budget");
        *slot = Some(frame);
        Ok(())
    }
}

/// Host state for one component instance.
pub struct WebGpuHost {
    instance: Arc<wgpu_core::global::Global>,
    table: ResourceTable,
    options: WasiWebGpuOptions,
    wasi: WasiCtx,
    records: NamespacedRecordsHost,
    presenter: Box<dyn CanvasGpuPresenter>,
}

/// A per-runtime records root, so two test runtimes never share a store.
fn default_records_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "soil-webgpu-records-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

impl WebGpuHost {
    pub fn new() -> Result<Self> {
        Self::with_presenter(Box::new(DiscardPresenter { size: (480, 640) }))
    }

    pub fn with_presenter(presenter: Box<dyn CanvasGpuPresenter>) -> Result<Self> {
        Self::with_presenter_and_records(presenter, default_records_root())
    }

    pub fn with_presenter_and_records(
        presenter: Box<dyn CanvasGpuPresenter>,
        records_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let records_root = records_root.as_ref();
        std::fs::create_dir_all(records_root)
            .with_context(|| format!("create records root {}", records_root.display()))?;
        let records = NamespacedRecordsHost::open(
            records_root,
            vec![Namespace::read(
                "vitals.view",
                "dev/personal/telemetry",
                "vitals.view",
            )],
        )
        .context("open records for the WebGPU guest")?;
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
            records,
            presenter,
        })
    }
}

impl records_host::Host for WebGpuHost {
    fn get(
        &mut self,
        key: records_host::Key,
    ) -> std::result::Result<records_host::Entry, records_host::Error> {
        records_host::Host::get(&mut self.records, key)
    }

    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> std::result::Result<records_host::Page, records_host::Error> {
        records_host::Host::scan(&mut self.records, collection, cursor, limit)
    }

    fn upsert(
        &mut self,
        entry: records_host::Entry,
    ) -> std::result::Result<records_host::Entry, records_host::Error> {
        records_host::Host::upsert(&mut self.records, entry)
    }

    fn compare_and_swap(
        &mut self,
        key: records_host::Key,
        expected: Option<records_host::Version>,
        entry: records_host::Entry,
    ) -> std::result::Result<records_host::CasResult, records_host::Error> {
        records_host::Host::compare_and_swap(&mut self.records, key, expected, entry)
    }

    fn delete(&mut self, key: records_host::Key) -> std::result::Result<(), records_host::Error> {
        records_host::Host::delete(&mut self.records, key)
    }

    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> std::result::Result<records_host::Page, records_host::Error> {
        records_host::Host::changes(&mut self.records, since, limit)
    }

    fn apply(
        &mut self,
        entries: Vec<records_host::Entry>,
    ) -> std::result::Result<records_host::ApplyResult, records_host::Error> {
        records_host::Host::apply(&mut self.records, entries)
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

impl soil::canvas_gpu::surface::Host for WebGpuHost {
    fn size(&mut self) -> (u32, u32) {
        self.presenter.size()
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
    records_root: PathBuf,
}

impl WebGpuRuntime {
    pub fn new() -> Result<Self> {
        Self::with_records(default_records_root())
    }

    /// Build a runtime whose guest reads `vitals.view` from this records root.
    pub fn with_records(records_root: impl Into<PathBuf>) -> Result<Self> {
        let records_root = records_root.into();
        std::fs::create_dir_all(&records_root)
            .with_context(|| format!("create records root {}", records_root.display()))?;
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.wasm_component_model_async(true);
        let engine = Engine::new(&config).map_err(anyhow::Error::from)?;
        let mut linker = Linker::new(&engine);
        // `wasi:io/poll` is the transport for component-model async, not an
        // authority grant. The WASI context has no preopens, env, args, or
        // network policy; a guest that imports filesystem or random gets an
        // empty implementation and cannot reach the host filesystem.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(anyhow::Error::from)
            .context("link wasi:io polling")?;
        wasi_webgpu_wasmtime::add_to_linker(&mut linker)
            .map_err(anyhow::Error::from)
            .context("link wasi:webgpu")?;
        soil::canvas_gpu::surface::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:canvas-gpu")?;
        soil_records_host::add_to_linker::<WebGpuHost>(&mut linker).context("link soil:records")?;
        Ok(Self {
            engine,
            linker,
            records_root,
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn linker(&self) -> &Linker<WebGpuHost> {
        &self.linker
    }

    /// Compile and run one guest component. The guest must export
    /// `run: async func() -> u32`; imports outside `wasi:webgpu`,
    /// `soil:canvas-gpu`, `soil:records`, and the empty WASI context fail to
    /// link.
    pub async fn run_component(&self, bytes: &[u8]) -> Result<u32> {
        self.run_component_with_presenter(bytes, Box::new(DiscardPresenter { size: (480, 640) }))
            .await
    }

    pub async fn run_component_with_presenter(
        &self,
        bytes: &[u8],
        presenter: Box<dyn CanvasGpuPresenter>,
    ) -> Result<u32> {
        let component = Component::new(&self.engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile WebGPU component")?;
        let mut store = Store::new(
            &self.engine,
            WebGpuHost::with_presenter_and_records(presenter, &self.records_root)?,
        );
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
    capture_with_runtime(&runtime, bytes, width, height)
}

/// Run with a capture sink and a caller-selected records root.
///
/// The guest can read only the `vitals.view` namespace from this root; no
/// filesystem path or store handle crosses the component boundary.
pub fn run_component_capture_with_records(
    bytes: &[u8],
    width: u32,
    height: u32,
    records_root: impl AsRef<Path>,
) -> Result<(u32, Option<CanvasFrame>)> {
    let runtime = WebGpuRuntime::with_records(records_root.as_ref().to_path_buf())?;
    capture_with_runtime(&runtime, bytes, width, height)
}

fn capture_with_runtime(
    runtime: &WebGpuRuntime,
    bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<(u32, Option<CanvasFrame>)> {
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
