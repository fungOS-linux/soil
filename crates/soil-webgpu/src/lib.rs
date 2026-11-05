//! Bounded `wasi:webgpu` host for Soil.
//!
//! This is intentionally a separate crate from `soil-runtime`: the Canvas
//! runtime is a core-Wasm host, while WebGPU is a component-model capability.
//! A Spore that imports `wasi:webgpu` gets exactly this linker and nothing
//! else. Adapter/device creation, validation, limits and queue submission stay
//! in the host through `wgpu-core`; no native handle crosses the boundary.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use base64::Engine as _;
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

/// A presenter that hands completed frames to a Canvas `ui.sock`.
///
/// This is the cross-process form of the same handoff: the guest never learns
/// the socket path, the host owns the connection and the lease name. The
/// protocol is Canvas's line-delimited `gpu_frame` message; pixels are base64
/// on this path because the socket is JSON, not a shared-memory transport.
pub struct SocketPresenter {
    stream: UnixStream,
    source_id: String,
    size: (u32, u32),
    revision: u64,
}

impl SocketPresenter {
    pub fn new(
        socket: impl AsRef<std::path::Path>,
        source_id: impl Into<String>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let source_id = source_id.into();
        anyhow::ensure!(
            !source_id.is_empty()
                && source_id.len() <= 64
                && source_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "Canvas source id must be a safe identifier"
        );
        let mut stream = UnixStream::connect(socket.as_ref())
            .with_context(|| format!("connect Canvas socket {}", socket.as_ref().display()))?;
        let hello = format!(
            r#"{{"kind":"hello","surface_id":"{source_id}","title":"{source_id}","width":{width},"height":{height}}}"#
        );
        stream.write_all(hello.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        Ok(Self {
            stream,
            source_id,
            size: (width, height),
            revision: 1,
        })
    }
}

impl CanvasGpuPresenter for SocketPresenter {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn present(&mut self, frame: CanvasFrame) -> Result<()> {
        anyhow::ensure!(
            frame.width == self.size.0 && frame.height == self.size.1,
            "guest presented {}x{}, host granted {}x{}",
            frame.width,
            frame.height,
            self.size.0,
            self.size.1
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(&frame.rgba);
        let revision = self.revision;
        self.revision = self.revision.saturating_add(1);
        let message = format!(
            r#"{{"kind":"gpu_frame","source_id":"{}","width":{},"height":{},"revision":{revision},"rgba":"{encoded}"}}"#,
            self.source_id, frame.width, frame.height
        );
        self.stream.write_all(message.as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;
        Ok(())
    }
}

/// Host state for one component instance.
pub struct WebGpuHost {
    instance: Arc<wgpu_core::global::Global>,
    table: ResourceTable,
    options: WasiWebGpuOptions,
    wasi: WasiCtx,
    presenter: Box<dyn CanvasGpuPresenter>,
}

impl WebGpuHost {
    pub fn new() -> Result<Self> {
        Self::with_presenter(Box::new(DiscardPresenter { size: (480, 640) }))
    }

    pub fn with_presenter(presenter: Box<dyn CanvasGpuPresenter>) -> Result<Self> {
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
            presenter,
        })
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
        soil::canvas_gpu::surface::add_to_linker::<_, HasSelf<_>>(&mut linker, |host| host)
            .map_err(anyhow::Error::from)
            .context("link soil:canvas-gpu")?;
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
        let mut store = Store::new(&self.engine, WebGpuHost::with_presenter(presenter)?);
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

/// Run one component and present its first frame to a Canvas `ui.sock`.
pub fn run_component_socket(
    bytes: &[u8],
    socket: impl AsRef<std::path::Path>,
    source_id: impl Into<String>,
    width: u32,
    height: u32,
) -> Result<u32> {
    let runtime = WebGpuRuntime::new()?;
    let presenter = SocketPresenter::new(socket, source_id, width, height)?;
    pollster::block_on(runtime.run_component_with_presenter(bytes, Box::new(presenter)))
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
