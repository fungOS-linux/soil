//! Threaded core-Wasm process lane for native-shaped Rust code.
//!
//! This is intentionally separate from the `wasm32-wasip2` component host:
//! cpal and Tokio-style code needs guest threads, and the component target's
//! libc is single-threaded.

mod audio;
mod threads;

use std::sync::Arc;

use anyhow::{Context, Result};
use wasmtime::{Config, Engine, Linker, Module, SharedMemory, Store};
use wasmtime_wasi::{WasiCtxBuilder, p1};

use audio::AudioHost;
pub use audio::AudioStats;
use threads::WasiThreadsCtx;

struct ProcessHost {
    wasi: p1::WasiP1Ctx,
    audio: Arc<AudioHost>,
    threads: Option<Arc<WasiThreadsCtx<ProcessHost>>>,
    memory: Option<SharedMemory>,
}

impl Clone for ProcessHost {
    fn clone(&self) -> Self {
        Self {
            // Threads inherit a fresh WASI table. The audio host and thread
            // factory are shared; stdio/preopens are intentionally not cloned.
            wasi: WasiCtxBuilder::new().inherit_stdio().build_p1(),
            audio: Arc::clone(&self.audio),
            threads: self.threads.clone(),
            memory: self.memory.clone(),
        }
    }
}

pub struct ProcessRuntime {
    engine: Engine,
    linker: Linker<ProcessHost>,
    module: Module,
    memory: SharedMemory,
    audio: Arc<AudioHost>,
}

impl ProcessRuntime {
    pub fn new(bytes: &[u8]) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_threads(true);
        config.shared_memory(true);
        let engine = Engine::new(&config).map_err(anyhow::Error::from)?;
        let module = Module::new(&engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile threaded process module")?;
        let audio = Arc::new(AudioHost::new());
        let mut linker = Linker::new(&engine);
        p1::add_to_linker_sync(&mut linker, |host: &mut ProcessHost| &mut host.wasi)
            .map_err(anyhow::Error::from)
            .context("link WASI p1")?;

        let store = Store::new(
            &engine,
            ProcessHost {
                wasi: WasiCtxBuilder::new().inherit_stdio().build_p1(),
                audio: Arc::clone(&audio),
                threads: None,
                memory: None,
            },
        );
        let memory = threads::define_shared_memory(&mut linker, &store, &module)?;
        threads::add_to_linker(&mut linker, |host| {
            host.threads.as_ref().expect("thread factory installed")
        })?;
        add_audio_to_linker(&mut linker)?;

        Ok(Self {
            engine,
            linker,
            module,
            memory,
            audio,
        })
    }

    pub fn run(&self) -> Result<()> {
        let instance_pre = Arc::new(
            self.linker
                .instantiate_pre(&self.module)
                .map_err(anyhow::Error::from)
                .context("prepare threaded process instance")?,
        );
        let threads = Arc::new(WasiThreadsCtx::from_instance_pre(Arc::clone(&instance_pre)));
        let mut store = Store::new(
            &self.engine,
            ProcessHost {
                wasi: WasiCtxBuilder::new().inherit_stdio().build_p1(),
                audio: Arc::clone(&self.audio),
                threads: Some(threads),
                memory: Some(self.memory.clone()),
            },
        );
        let instance = instance_pre
            .instantiate(&mut store)
            .map_err(anyhow::Error::from)
            .context("instantiate threaded process")?;
        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(anyhow::Error::from)
            .context("required export _start")?;
        start
            .call(&mut store, ())
            .map_err(anyhow::Error::from)
            .context("run threaded process")?;
        Ok(())
    }

    pub fn audio_stats(&self) -> AudioStats {
        self.audio.stats()
    }
}

fn add_audio_to_linker(linker: &mut Linker<ProcessHost>) -> Result<()> {
    linker.func_wrap(
        "soil_audio",
        "soil_audio_open",
        |caller: wasmtime::Caller<'_, ProcessHost>, sample_rate: u32, channels: u32| -> i32 {
            let audio = Arc::clone(&caller.data().audio);
            audio
                .open(sample_rate, u16::try_from(channels).unwrap_or(0))
                .unwrap_or_else(|error| {
                    eprintln!("soil_audio_open: {error:#}");
                    -1
                })
        },
    )?;
    linker.func_wrap(
        "soil_audio",
        "soil_audio_write",
        |caller: wasmtime::Caller<'_, ProcessHost>, handle: i32, ptr: u32, frames: u32| -> i32 {
            let channels = match caller.data().audio.channels(handle) {
                Some(channels) => channels,
                None => return 0,
            };
            let samples = frames as usize * channels;
            let bytes = samples * std::mem::size_of::<f32>();
            let Some(memory) = caller.data().memory.as_ref() else {
                return -1;
            };
            let data = memory.data();
            let start = ptr as usize;
            let Some(raw) = data.get(start..start + bytes) else {
                return -1;
            };
            let mut pcm = Vec::with_capacity(samples);
            for chunk in raw.as_chunks::<4>().0 {
                // SAFETY: the guest is paused in this host import and the
                // shared memory is stable for the duration of the call.
                let bytes = unsafe {
                    [
                        *chunk[0].get(),
                        *chunk[1].get(),
                        *chunk[2].get(),
                        *chunk[3].get(),
                    ]
                };
                pcm.push(f32::from_le_bytes(bytes));
            }
            let audio = Arc::clone(&caller.data().audio);
            i32::try_from(audio.write(handle, &pcm, channels)).unwrap_or(-1)
        },
    )?;
    linker.func_wrap(
        "soil_audio",
        "soil_audio_close",
        |caller: wasmtime::Caller<'_, ProcessHost>, handle: i32| {
            let audio = Arc::clone(&caller.data().audio);
            audio.close(handle);
        },
    )?;
    Ok(())
}
