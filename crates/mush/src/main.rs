//! Native host for the Mush Spore.
//!
//! The component owns shell state and command semantics. This process owns the
//! VFS root, the `soil:vfs` / `soil:index` / `soil:vfs-mut` grants, and the
//! line-oriented terminal.

mod config;
mod gateway;
mod mcp_gateway;
mod mcp_server;
mod overlay;
mod service;

use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use rand::Rng;
use rustyline::{DefaultEditor, error::ReadlineError};
use soil::command::command as command_abi;
use soil_command::{CommandRegistry, CommandRunner, RecordsBinding, RunRequest};
use soil_inference::{GeodeResolver, SoilConfig};
use soil_vfs_host::{VfsHost, index, vfs, vfs_mut};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({ path: "../../spores/mush/wit", world: "mush" });

/// The host binary carries the Mush Spore by default, so an installed `mush`
/// works without a sidecar file. `--wasm PATH` remains available for dev.
const EMBEDDED_MUSH: &[u8] = include_bytes!("../../../spores/mush/module.wasm");

struct Host {
    vfs: VfsHost,
    wasi: WasiCtx,
    table: ResourceTable,
    commands: CommandHost,
}

struct CommandHost {
    root: PathBuf,
    runner: CommandRunner,
}

impl CommandHost {
    fn new(root: PathBuf, records: Option<RecordsBinding>) -> Result<Self> {
        Ok(Self {
            root,
            runner: CommandRunner::new(records)?,
        })
    }

    fn registry(&self) -> Result<CommandRegistry> {
        if self.root.is_dir() {
            CommandRegistry::scan(&self.root)
        } else {
            Ok(CommandRegistry::empty(&self.root))
        }
    }

    fn list(&self) -> Result<Vec<command_abi::Command>> {
        Ok(self
            .registry()?
            .entries()
            .map(|entry| command_abi::Command {
                name: entry.name.clone(),
                version: entry.version.clone(),
                summary: entry.summary.clone(),
            })
            .collect())
    }

    fn path(&self) -> Result<Vec<String>> {
        Ok(self
            .registry()?
            .path()
            .into_iter()
            .map(str::to_owned)
            .collect())
    }

    fn run(
        &self,
        name: &str,
        args: Vec<String>,
        stdin: Vec<u8>,
        cwd: Option<String>,
    ) -> Result<command_abi::Output> {
        let registry = self.registry()?;
        let entry = registry
            .get(name)
            .cloned()
            .with_context(|| format!("command not found: {name}"))?;
        let mut env = vec![("TERM".into(), "dumb".into())];
        if let Some(cwd) = &cwd {
            env.push(("PWD".into(), cwd.clone()));
        }
        let output = self.runner.run(
            &entry,
            &RunRequest {
                args,
                stdin,
                env,
                cwd,
            },
        )?;
        Ok(command_abi::Output {
            code: output.code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

impl Host {
    fn new(
        root: &std::path::Path,
        command_root: PathBuf,
        records: Option<RecordsBinding>,
    ) -> Result<Self> {
        Ok(Self {
            vfs: VfsHost::open(root)?,
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            commands: CommandHost::new(command_root, records)?,
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

impl command_abi::Host for Host {
    fn commands(&mut self) -> Vec<command_abi::Command> {
        self.commands.list().unwrap_or_default()
    }

    fn path(&mut self) -> Vec<String> {
        self.commands.path().unwrap_or_default()
    }

    fn run(
        &mut self,
        name: String,
        args: Vec<String>,
        stdin: Vec<u8>,
        cwd: Option<String>,
    ) -> Result<command_abi::Output, String> {
        self.commands
            .run(&name, args, stdin, cwd)
            .map_err(|error| format!("{error:#}"))
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

pub(crate) struct Runtime {
    store: Store<Host>,
    instance: Mush,
}

impl Runtime {
    pub(crate) fn open(
        root: &std::path::Path,
        wasm: &[u8],
        term: &str,
        command_root: PathBuf,
        records: Option<RecordsBinding>,
        principal: Option<&str>,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        soil_vfs_host::add_to_linker::<Host>(&mut linker)?;
        soil_vfs_host::add_mutation_to_linker::<Host>(&mut linker)?;
        command_abi::add_to_linker::<Host, HasSelf<Host>>(&mut linker, |host| host)?;
        let component = Component::new(&engine, wasm)?;
        let host = Host::new(root, command_root, records)?;
        let mut store = Store::new(&engine, host);
        let instance = Mush::instantiate(&mut store, &component, &linker)?;
        let mut args = vec![format!("--term={term}")];
        if let Some(principal) = principal {
            args.push(format!("--principal={principal}"));
        }
        let response = instance
            .call_start(&mut store, &args)?
            .map_err(anyhow::Error::msg)?;
        print!("{}", response.output);
        eprint!("{}", response.stderr);
        io::stdout().flush()?;
        Ok(Self { store, instance })
    }

    pub(crate) fn execute(&mut self, line: &str) -> Result<Response> {
        self.instance
            .call_execute(&mut self.store, line)?
            .map_err(anyhow::Error::msg)
    }

    pub(crate) fn prompt(&mut self) -> Result<String> {
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
    if args.first().map(String::as_str) == Some("--version") {
        println!("mush {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--help") {
        println!(
            "mush {}\n\nUsage:\n  mush [OPTIONS]\n  mush doctor [OPTIONS]\n  mush run op [OPTIONS]\n  mush --serve --socket PATH --root DIR [OPTIONS]\n\nThe Mush Spore is embedded; --wasm overrides it for development.\n\nOptions:\n  --root DIR                 VFS root (default: ~/.local/share/fungos/vfs)\n  --wasm PATH                development Spore override\n  --command-root DIR         wasm command package root\n  --records-root DIR         Soil records root\n  --records-book BOOK        records book, e.g. personal/avery/marbles\n  --records-collection NAME  records collection name\n  --principal ID             signed principal label for this session\n  --socket PATH              Unix socket for --serve\n  --config PATH              config file (default: ~/.config/fungos/mush/config.toml)\n  --term TERM                terminal type passed to the Spore\n  --command LINE             execute one or more command lines\n  --script PATH              source a VFS script",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let doctor = args.first().map(String::as_str) == Some("doctor");
    if doctor {
        args.remove(0);
    }
    let mcp = args.first().map(String::as_str) == Some("mcp");
    if mcp {
        args.remove(0);
    }
    let serve = args.iter().any(|arg| arg == "--serve");
    args.retain(|arg| arg != "--serve");
    let config_path = take_option(&mut args, "--config")
        .map(|value| config::expand_home(&value))
        .unwrap_or_else(config::default_path);
    let config = config::load(&config_path)?;
    if args.first().map(String::as_str) == Some("request") {
        args.remove(0);
        return run_request(&config, args);
    }
    let root = take_option(&mut args, "--root")
        .or_else(|| std::env::var("MUSH_ROOT").ok())
        .or_else(|| config.root.as_ref().map(|path| path.display().to_string()))
        .unwrap_or_else(|| "~/.local/share/fungos/vfs".into());
    let root = config::expand_home(&root);

    if args.first().map(String::as_str) == Some("run")
        && args.get(1).map(String::as_str) == Some("op")
    {
        args.drain(0..2);
        return run_operator(&root, args);
    }
    let wasm = take_option(&mut args, "--wasm")
        .or_else(|| std::env::var("MUSH_WASM").ok())
        .map(|value| config::expand_home(&value));
    let term = take_option(&mut args, "--term")
        .or_else(|| std::env::var("TERM").ok())
        .unwrap_or_else(|| "xterm-256color".into());
    let script = take_option(&mut args, "--script");
    let command = take_option(&mut args, "--command");
    let history = take_option(&mut args, "--history")
        .or_else(|| std::env::var("MUSH_HISTORY").ok())
        .map(|value| config::expand_home(&value))
        .unwrap_or_else(|| root.join(".fungos/mush-history"));
    let command_root = take_option(&mut args, "--command-root")
        .or_else(|| std::env::var("FUNGOS_COMMAND_ROOT").ok())
        .or_else(|| {
            config
                .command_root
                .as_ref()
                .map(|path| path.display().to_string())
        })
        .map(|value| config::expand_home(&value))
        .unwrap_or_else(|| config::expand_home("~/.local/share/fungos/commands"));
    let records_root = take_option(&mut args, "--records-root")
        .or_else(|| std::env::var("FUNGOS_RECORDS_ROOT").ok())
        .or_else(|| {
            config
                .records_root
                .as_ref()
                .map(|path| path.display().to_string())
        })
        .map(|value| config::expand_home(&value));
    let records_book = take_option(&mut args, "--records-book")
        .or_else(|| std::env::var("FUNGOS_RECORDS_BOOK").ok())
        .or_else(|| config.records_book.clone());
    let records_collection = take_option(&mut args, "--records-collection")
        .or_else(|| std::env::var("FUNGOS_RECORDS_COLLECTION").ok())
        .or_else(|| config.records_collection.clone());
    let records = match (records_root, records_book, records_collection) {
        (Some(root), Some(book), Some(collection)) => Some(RecordsBinding {
            root,
            book,
            collection,
        }),
        (None, None, None) => None,
        _ => anyhow::bail!(
            "records binding needs --records-root, --records-book and --records-collection"
        ),
    };
    let principal = take_option(&mut args, "--principal")
        .or_else(|| std::env::var("MUSH_PRINCIPAL").ok())
        .or_else(|| config.principal.clone());
    let socket = take_option(&mut args, "--socket")
        .or_else(|| std::env::var("MUSH_SOCKET").ok())
        .or_else(|| {
            config
                .socket
                .as_ref()
                .map(|path| path.display().to_string())
        })
        .map(|value| config::expand_home(&value));
    let listen = take_option(&mut args, "--listen")
        .or_else(|| std::env::var("MUSH_MCP_LISTEN").ok())
        .unwrap_or_else(|| "127.0.0.1:0".into());
    let configured_token =
        take_option(&mut args, "--mcp-token").or_else(|| std::env::var("MUSH_MCP_TOKEN").ok());
    let mcp_token = configured_token.clone().unwrap_or_else(new_mcp_token);

    if doctor {
        return doctor_report(
            &config_path,
            &root,
            &command_root,
            records.as_ref(),
            wasm.as_deref(),
            principal.as_deref(),
            socket.as_deref(),
        );
    }

    let bytes = match wasm {
        Some(wasm) => std::fs::read(&wasm)
            .with_context(|| format!("read Mush component {}", wasm.display()))?,
        None => EMBEDDED_MUSH.to_vec(),
    };
    let mut runtime = Runtime::open(
        &root,
        &bytes,
        &term,
        command_root,
        records,
        principal.as_deref(),
    )?;

    if serve {
        let socket = socket
            .as_deref()
            .context("mush --serve needs --socket PATH or a config socket")?;
        return service::serve(socket, |line| {
            let response = runtime.execute(line)?;
            Ok(service::Executed {
                output: response.output,
                stderr: response.stderr,
                exit: response.exit,
                status: response.status,
            })
        });
    }

    if mcp {
        let server = tiny_http::Server::http(&listen)
            .map_err(|error| anyhow::anyhow!("mcp listen {listen}: {error}"))?;
        if configured_token.is_none() {
            eprintln!("mush: mcp bearer token {mcp_token}");
        }
        return mcp_server::serve(server, &mcp_token, |line| {
            let response = runtime.execute(line)?;
            Ok((
                format!("{}{}", response.output, response.stderr),
                response.exit,
                response.status,
            ))
        });
    }

    if let Some(command) = command {
        for line in command.lines() {
            let response = runtime.execute(line)?;
            print!("{}", response.output);
            eprint!("{}", response.stderr);
            if response.exit {
                return Ok(());
            }
        }
        return Ok(());
    }

    if let Some(script) = script {
        let response = runtime.execute(&format!("source {script}"))?;
        print!("{}", response.output);
        eprint!("{}", response.stderr);
        return Ok(());
    }

    if io::stdin().is_terminal() {
        run_interactive(&mut runtime, &history)
    } else {
        run_lines(&mut runtime)
    }
}

fn new_mcp_token() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn run_request(config: &config::Config, mut args: Vec<String>) -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let socket = take_option(&mut args, "--socket")
        .or_else(|| std::env::var("MUSH_SOCKET").ok())
        .or_else(|| {
            config
                .socket
                .as_ref()
                .map(|path| path.display().to_string())
        })
        .map(|value| config::expand_home(&value))
        .context("mush request needs --socket PATH or a config socket")?;
    let json = args.iter().any(|arg| arg == "--json");
    args.retain(|arg| arg != "--json");
    if args.is_empty() {
        anyhow::bail!("usage: mush request [--socket PATH] [--json] COMMAND...");
    }
    let command = args.join(" ");
    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("connect Mush service {}", socket.display()))?;
    stream
        .write_all(serde_json::to_string(&serde_json::json!({"command": command}))?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: serde_json::Value = serde_json::from_str(&line).context("parse Mush response")?;
    if json {
        print!("{line}");
        if !line.ends_with('\n') {
            println!();
        }
    } else if response.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        if let Some(output) = response.get("output").and_then(serde_json::Value::as_str) {
            print!("{output}");
        }
        if let Some(stderr) = response.get("stderr").and_then(serde_json::Value::as_str) {
            eprint!("{stderr}");
        }
    } else {
        let error = response
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Mush service request failed");
        anyhow::bail!("{error}");
    }
    io::stdout().flush()?;
    let status = response
        .get("status")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0) as i32;
    if status != 0 {
        std::process::exit(status);
    }
    Ok(())
}

fn doctor_report(
    config_path: &Path,
    root: &Path,
    command_root: &Path,
    records: Option<&RecordsBinding>,
    wasm: Option<&Path>,
    principal: Option<&str>,
    socket: Option<&Path>,
) -> Result<()> {
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let uid = unsafe { libc::geteuid() };
    println!("mush {}", env!("CARGO_PKG_VERSION"));
    println!("user: {user} (uid {uid})");
    println!("config: {}", config_path.display());
    let mut problems = Vec::new();
    if root.is_dir() {
        println!("vfs root: {} [ok]", root.display());
    } else {
        println!("vfs root: {} [missing]", root.display());
        problems.push("VFS root is missing");
    }
    if command_root.is_dir() {
        let count = CommandRegistry::scan(command_root)
            .map(|registry| registry.entries().count())
            .unwrap_or(0);
        println!(
            "command root: {} [{count} commands]",
            command_root.display()
        );
    } else {
        println!("command root: {} [missing]", command_root.display());
        problems.push("command root is missing");
    }
    match records {
        Some(records) => {
            if records.root.is_dir() {
                println!(
                    "records: {} book={} collection={} [ok]",
                    records.root.display(),
                    records.book,
                    records.collection
                );
            } else {
                println!("records: {} [missing]", records.root.display());
                problems.push("records root is missing");
            }
        }
        None => println!("records: disabled"),
    }
    match wasm {
        Some(path) if path.is_file() => println!("spore: {} [ok]", path.display()),
        Some(path) => {
            println!("spore: {} [missing]", path.display());
            problems.push("Spore override is missing");
        }
        None => println!("spore: embedded"),
    }
    println!(
        "principal: {}",
        principal.unwrap_or("(not set; using the POSIX user)")
    );
    match socket {
        Some(path) if path.exists() => println!("service socket: {} [exists]", path.display()),
        Some(path) => println!("service socket: {} [not running]", path.display()),
        None => println!("service socket: disabled"),
    }
    if problems.is_empty() {
        println!("status: ok");
        Ok(())
    } else {
        anyhow::bail!(
            "doctor found {} problem(s): {}",
            problems.len(),
            problems.join(", ")
        )
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
                eprint!("{}", response.stderr);
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
        eprint!("{}", response.stderr);
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
