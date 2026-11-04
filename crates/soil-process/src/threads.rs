//! Minimal implementation of the `wasi-threads` host ABI.
//!
//! Adapted from `wasmtime-wasi-threads` 46.0.3, which is the reference
//! implementation of `wasi.thread-spawn` for Wasmtime core modules.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
    thread,
};

use anyhow::{Result, bail};
use wasmtime::{Caller, ExternType, InstancePre, Linker, Module, SharedMemory, Store};

const WASI_ENTRY_POINT: &str = "wasi_thread_start";

pub struct WasiThreadsCtx<T> {
    instance_pre: Arc<InstancePre<T>>,
    tid: AtomicI32,
}

impl<T: Clone + Send + 'static> WasiThreadsCtx<T> {
    pub fn from_instance_pre(instance_pre: Arc<InstancePre<T>>) -> Self {
        Self {
            instance_pre,
            tid: AtomicI32::new(0),
        }
    }

    fn spawn(&self, host: T, thread_start_arg: i32) -> Result<i32> {
        let instance_pre = Arc::clone(&self.instance_pre);
        if !has_entry_point(instance_pre.module()) {
            bail!("missing {WASI_ENTRY_POINT} export");
        }
        if !has_correct_signature(instance_pre.module()) {
            bail!("{WASI_ENTRY_POINT} must have signature (i32, i32) -> ()");
        }
        let Some(wasi_thread_id) = self.next_thread_id() else {
            bail!("ran out of valid wasi thread IDs");
        };

        thread::Builder::new()
            .name(format!("wasi-thread-{wasi_thread_id}"))
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let mut store = Store::new(instance_pre.module().engine(), host);
                    let instance = instance_pre.instantiate(&mut store).unwrap();
                    let entry = instance
                        .get_typed_func::<(i32, i32), ()>(&mut store, WASI_ENTRY_POINT)
                        .unwrap();
                    if let Err(error) = entry.call(&mut store, (wasi_thread_id, thread_start_arg)) {
                        eprintln!("wasi-thread-{wasi_thread_id}: {error}");
                        std::process::exit(1);
                    }
                }));
                if let Err(panic) = result {
                    eprintln!("wasi-thread-{wasi_thread_id} panicked: {panic:?}");
                    std::process::exit(1);
                }
            })?;
        Ok(wasi_thread_id)
    }

    fn next_thread_id(&self) -> Option<i32> {
        match self
            .tid
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| match v {
                ..=0x1fff_fffe => Some(v + 1),
                _ => None,
            }) {
            Ok(v) => Some(v + 1),
            Err(_) => None,
        }
    }
}

/// Define the shared memory imported by a `wasm32-wasip1-threads` module.
pub fn define_shared_memory<T>(
    linker: &mut Linker<T>,
    store: &Store<T>,
    module: &Module,
) -> Result<SharedMemory> {
    for import in module.imports() {
        if let Some(memory) = import.ty().memory() {
            if !memory.is_shared() {
                bail!("wasi-threads module must import a shared memory");
            }
            let shared = SharedMemory::new(module.engine(), memory.clone())?;
            linker.define(store, import.module(), import.name(), shared.clone())?;
            return Ok(shared);
        }
    }
    bail!("wasi-threads module does not import a memory")
}

/// Add `wasi.thread-spawn` to a linker. The context is read from the store at
/// call time, so the same linker can instantiate the main and child threads.
pub fn add_to_linker<T: Clone + Send + 'static>(
    linker: &mut Linker<T>,
    get_cx: impl Fn(&mut T) -> &WasiThreadsCtx<T> + Send + Sync + Copy + 'static,
) -> Result<()> {
    linker.func_wrap(
        "wasi",
        "thread-spawn",
        move |mut caller: Caller<'_, T>, start_arg: i32| -> i32 {
            let host = caller.data().clone();
            match get_cx(caller.data_mut()).spawn(host, start_arg) {
                Ok(tid) => tid,
                Err(error) => {
                    eprintln!("wasi thread-spawn: {error:#}");
                    -1
                }
            }
        },
    )?;
    Ok(())
}

fn has_entry_point(module: &Module) -> bool {
    module.get_export(WASI_ENTRY_POINT).is_some()
}

fn has_correct_signature(module: &Module) -> bool {
    match module.get_export(WASI_ENTRY_POINT) {
        Some(ExternType::Func(ty)) => {
            ty.params().len() == 2
                && ty.params().next().is_some_and(|p| p.is_i32())
                && ty.params().nth(1).is_some_and(|p| p.is_i32())
                && ty.results().len() == 0
        }
        _ => false,
    }
}
