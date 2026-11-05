//! Native host for the Mush Spore.
//!
//! The component owns shell state and command semantics. This process owns the
//! VFS root, the `soil:vfs` / `soil:index` / `soil:vfs-mut` grants, and the
//! line-oriented terminal.

use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use soil_vfs_host::{VfsHost, index, vfs, vfs_mut};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({ path: "../../spores/mush/wit", world: "mush" });

struct Host {
    vfs: VfsHost,
    wasi: WasiCtx,
    table: ResourceTable,
}

impl Host {
    fn new(root: &std::path::Path) -> Result<Self> {
        Ok(Self {
            vfs: VfsHost::open(root)?,
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
        })
    }
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl vfs::Host for Host {
    fn ls(&mut self, parent: String, offset: u64, limit: u32) -> Result<vfs::Page, vfs::Error> {
        self.vfs.ls(parent, offset, limit)
    }

    fn stat(&mut self, path: String) -> Result<vfs::Entry, vfs::Error> {
        self.vfs.stat(path)
    }

    fn read(&mut self, path: String, offset: u64, length: u32) -> Result<vfs::Chunk, vfs::Error> {
        self.vfs.read(path, offset, length)
    }
}

impl index::Host for Host {
    fn browse(
        &mut self,
        parent: String,
        order: index::Order,
        offset: u64,
        limit: u32,
    ) -> Result<index::Page, index::Error> {
        self.vfs.browse(parent, order, offset, limit)
    }

    fn search(&mut self, query: index::Query) -> Result<index::Page, index::Error> {
        self.vfs.search(query)
    }

    fn knn(&mut self, query: index::KnnQuery) -> Result<index::Page, index::Error> {
        self.vfs.knn(query)
    }

    fn state(&mut self) -> Result<index::IndexState, index::Error> {
        self.vfs.state()
    }

    fn refresh(&mut self) -> Result<index::IndexState, index::Error> {
        self.vfs.refresh()
    }
}

impl vfs_mut::Host for Host {
    fn write(
        &mut self,
        path: String,
        data: Vec<u8>,
        media_type: String,
        expected_version: Option<u64>,
    ) -> Result<vfs_mut::WriteResult, vfs_mut::Error> {
        self.vfs.write(path, data, media_type, expected_version)
    }

    fn remove(&mut self, path: String) -> Result<(), vfs_mut::Error> {
        self.vfs.remove(path)
    }

    fn mkdir(&mut self, path: String) -> Result<(), vfs_mut::Error> {
        self.vfs.mkdir(path)
    }

    fn rmdir(&mut self, path: String) -> Result<(), vfs_mut::Error> {
        self.vfs.rmdir(path)
    }

    fn rename(
        &mut self,
        source: String,
        target: String,
    ) -> Result<vfs_mut::WriteResult, vfs_mut::Error> {
        self.vfs.rename(source, target)
    }
}

struct Runtime {
    store: Store<Host>,
    instance: Mush,
}

impl Runtime {
    fn open(root: &std::path::Path, wasm: &[u8], term: &str) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        soil_vfs_host::add_to_linker::<Host>(&mut linker)?;
        soil_vfs_host::add_mutation_to_linker::<Host>(&mut linker)?;
        let component = Component::new(&engine, wasm)?;
        let host = Host::new(root)?;
        let mut store = Store::new(&engine, host);
        let instance = Mush::instantiate(&mut store, &component, &linker)?;
        let args = vec![format!("--term={term}")];
        let response = instance
            .call_start(&mut store, &args)?
            .map_err(anyhow::Error::msg)?;
        print!("{}", response.output);
        io::stdout().flush()?;
        Ok(Self { store, instance })
    }

    fn execute(&mut self, line: &str) -> Result<Response> {
        self.instance
            .call_execute(&mut self.store, line)?
            .map_err(anyhow::Error::msg)
    }

    fn prompt(&mut self) -> Result<String> {
        Ok(self.instance.call_prompt(&mut self.store)?)
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("mush: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let root = take_option(&mut args, "--root")
        .or_else(|| std::env::var("MUSH_ROOT").ok())
        .unwrap_or_else(|| "~/.local/share/fungos/vfs".into());
    let root = expand_home(&root);
    let wasm = take_option(&mut args, "--wasm")
        .or_else(|| std::env::var("MUSH_WASM").ok())
        .unwrap_or_else(|| "spores/mush/module.wasm".into());
    let wasm = expand_home(&wasm);
    let term = take_option(&mut args, "--term")
        .or_else(|| std::env::var("TERM").ok())
        .unwrap_or_else(|| "xterm-256color".into());
    let script = take_option(&mut args, "--script");
    let command = take_option(&mut args, "--command");
    let bytes =
        std::fs::read(&wasm).with_context(|| format!("read Mush component {}", wasm.display()))?;
    let mut runtime = Runtime::open(&root, &bytes, &term)?;

    if let Some(command) = command {
        for line in command.lines() {
            let response = runtime.execute(line)?;
            print!("{}", response.output);
            if response.exit {
                return Ok(());
            }
        }
        return Ok(());
    }

    if let Some(script) = script {
        let response = runtime.execute(&format!("source {script}"))?;
        print!("{}", response.output);
        return Ok(());
    }

    let stdin = io::stdin();
    loop {
        print!("{}", runtime.prompt()?);
        io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let response = runtime.execute(line.trim_end_matches(['\r', '\n']))?;
        print!("{}", response.output);
        io::stdout().flush()?;
        if response.exit {
            break;
        }
    }
    Ok(())
}

fn take_option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == name)?;
    if index + 1 >= args.len() {
        return None;
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Some(value)
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest);
    }
    PathBuf::from(value)
}
