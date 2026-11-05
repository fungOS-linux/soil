//! Native host for the Mush Spore.
//!
//! The component owns shell state and command semantics. This process owns the
//! VFS root, the `soil:vfs` / `soil:index` / `soil:vfs-mut` grants, and the
//! line-oriented terminal.

mod gateway;
mod mcp_gateway;
mod overlay;

use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use rustyline::{DefaultEditor, error::ReadlineError};
use soil_inference::{GeodeResolver, SoilConfig};
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

    if args.first().map(String::as_str) == Some("run")
        && args.get(1).map(String::as_str) == Some("op")
    {
        args.drain(0..2);
        return run_operator(&root, args);
    }
    let wasm = take_option(&mut args, "--wasm")
        .or_else(|| std::env::var("MUSH_WASM").ok())
        .unwrap_or_else(|| "spores/mush/module.wasm".into());
    let wasm = expand_home(&wasm);
    let term = take_option(&mut args, "--term")
        .or_else(|| std::env::var("TERM").ok())
        .unwrap_or_else(|| "xterm-256color".into());
    let script = take_option(&mut args, "--script");
    let command = take_option(&mut args, "--command");
    let history = take_option(&mut args, "--history")
        .or_else(|| std::env::var("MUSH_HISTORY").ok())
        .map(|value| expand_home(&value))
        .unwrap_or_else(|| root.join(".fungos/mush-history"));
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

    if io::stdin().is_terminal() {
        run_interactive(&mut runtime, &history)
    } else {
        run_lines(&mut runtime)
    }
}

fn run_interactive(runtime: &mut Runtime, history: &Path) -> Result<()> {
    let mut editor = DefaultEditor::new()?;
    let _ = editor.load_history(history);
    loop {
        let prompt = runtime.prompt()?;
        match editor.readline(&prompt) {
            Ok(line) => {
                let line = line.trim_end_matches(['\r', '\n']);
                if !line.trim().is_empty() {
                    let _ = editor.add_history_entry(line);
                }
                let response = runtime.execute(line)?;
                print!("{}", response.output);
                io::stdout().flush()?;
                if response.exit {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!();
            }
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(error.into()),
        }
    }
    if let Some(parent) = history.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = editor.save_history(history);
    Ok(())
}

fn run_lines(runtime: &mut Runtime) -> Result<()> {
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

fn run_operator(root: &Path, mut args: Vec<String>) -> Result<()> {
    let passthrough = args
        .iter()
        .position(|arg| arg == "--")
        .map(|index| args.split_off(index + 1));
    if args.last().map(String::as_str) == Some("--") {
        args.pop();
    }

    let config_path = take_option(&mut args, "--config").map(|value| expand_home(&value));
    let profile = take_option(&mut args, "--inference-profile")
        .or_else(|| std::env::var("MUSH_INFERENCE_PROFILE").ok());
    let config = match config_path {
        Some(path) => SoilConfig::load(&path)?,
        None => SoilConfig::discover(root)?
            .map(|(_, config)| config)
            .unwrap_or_default(),
    };
    let mut resolved = config.resolve(profile.as_deref())?;
    if resolved.api_key.is_none()
        && let Some(credential) = &resolved.credential
    {
        resolved.api_key = GeodeResolver::new(config.geode.clone()).resolve(credential)?;
    }
    if let Some(value) = take_option(&mut args, "--base-url") {
        resolved.base_url = Some(value);
    }
    if let Some(value) = take_option(&mut args, "--model") {
        resolved.model = Some(value);
    }
    if let Some(value) = take_option(&mut args, "--mcp-url") {
        resolved.mcp_url = Some(value);
    }
    let operator = take_option(&mut args, "--operator-bin")
        .or(resolved.operator_bin.clone())
        .or_else(|| std::env::var("MUSH_OPERATOR").ok())
        .unwrap_or_else(|| "operator-tui".into());
    let prompt = take_option(&mut args, "--prompt");
    let keep = args.iter().any(|arg| arg == "--keep");
    let no_apply = args.iter().any(|arg| arg == "--no-apply");
    let no_gateway = args.iter().any(|arg| arg == "--no-gateway");
    args.retain(|arg| arg != "--keep" && arg != "--no-apply" && arg != "--no-gateway");
    let mut base_url = resolved.base_url.clone();
    let mut api_key = resolved.api_key.clone();
    let scrubber = std::sync::Arc::new(std::sync::Mutex::new(soil_scrub::Scrubber::new()));
    let _gateway = if !no_gateway {
        if let Some(upstream) = base_url.clone() {
            let gateway = gateway::start(upstream, api_key.clone(), Some(scrubber.clone()))?;
            eprintln!("mush: inference gateway {}", gateway.base_url);
            base_url = Some(gateway.base_url.clone());
            api_key = Some(gateway.token.clone());
            Some(gateway)
        } else {
            None
        }
    } else {
        None
    };
    let mut mcp_url = resolved.mcp_url.clone();
    let mut mcp_token = resolved.mcp_token.clone();
    let _mcp_gateway = if let Some(upstream) = mcp_url.clone() {
        let gateway = mcp_gateway::start(
            upstream,
            mcp_token.clone(),
            None,
            Some(scrubber.clone()),
            resolved.mcp_allowed_tools.clone(),
        )?;
        eprintln!("mush: mcp gateway {}", gateway.url);
        mcp_url = Some(gateway.url.clone());
        mcp_token = Some(gateway.token.clone());
        Some(gateway)
    } else {
        None
    };
    let overlay = overlay::Overlay::materialize(root)?;
    let workspace = overlay.path().to_path_buf();
    eprintln!("mush: overlay {}", workspace.display());

    let mut command = Command::new(&operator);
    command.arg("--workspace").arg(&workspace);
    command.env("MUSH_ROOT", root);
    command.env("MUSH_OVERLAY", &workspace);
    if let Some(provider) = &resolved.provider {
        command.env("MUSH_INFERENCE_PROVIDER", provider);
    }
    if let Some(base_url) = &base_url {
        command.env("BIFROST_BASE_URL", base_url);
    }
    if let Some(api_key) = &api_key {
        command.env("BIFROST_API_KEY", api_key);
    }
    if let Some(model) = &resolved.model {
        command.env("OPERATOR_MODEL", model);
    }
    if let Some(mcp_url) = &mcp_url {
        command.env("EDGERUNNER_MCP_URL", mcp_url);
    }
    if let Some(token) = &mcp_token {
        command.env("EDGERUNNER_MCP_TOKEN", token);
    }
    if let Some(prompt) = prompt {
        command.arg("--prompt").arg(prompt);
    }
    command.args(&args);
    if let Some(passthrough) = passthrough {
        command.args(passthrough);
    }
    let status = command.status().context("start operator session")?;

    let changes = overlay.diff()?;
    for change in &changes {
        eprintln!("mush: {:?} {}", change.kind, change.path);
    }
    if no_apply {
        eprintln!("mush: overlay discarded ({})", changes.len());
    } else {
        let report = overlay.apply(root)?;
        eprintln!(
            "mush: applied {} written, {} deleted, {} directories",
            report.written, report.deleted, report.directories
        );
    }
    if keep {
        let kept = overlay.keep();
        eprintln!("mush: kept {}", kept.display());
    }
    if !status.success() {
        anyhow::bail!("operator exited with {status}");
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
