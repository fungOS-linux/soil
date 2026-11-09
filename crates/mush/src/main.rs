//! Native host for the Mush Spore.
//!
//! The component owns shell state and command semantics. This process owns the
//! VFS root, the `soil:vfs` / `soil:index` / `soil:vfs-mut` grants, and the
//! line-oriented terminal.

mod complete;
mod config;
mod deadline;
mod desktop;
mod gateway;
mod job;
mod mcp_gateway;
mod mcp_server;
mod origin;
mod outcome;
mod overlay;
mod project;
mod service;
mod shell;
mod worktree;

use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use rand::Rng;
use rustyline::{Editor, error::ReadlineError, history::FileHistory};
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
    engine: Engine,
    /// Deadline applied to every command, or `None` when a human is driving.
    budget: Option<std::time::Duration>,
}

impl Runtime {
    pub(crate) fn open(
        root: &std::path::Path,
        wasm: &[u8],
        term: &str,
        command_root: PathBuf,
        records: Option<RecordsBinding>,
        principal: Option<&str>,
        desktop: Option<&desktop::Desktop>,
        budget: Option<std::time::Duration>,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        // Epoch interruption is what lets a timer stop a command that outlives
        // its caller's patience instead of waiting for it forever.
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        soil_vfs_host::add_to_linker::<Host>(&mut linker)?;
        soil_vfs_host::add_mutation_to_linker::<Host>(&mut linker)?;
        command_abi::add_to_linker::<Host, HasSelf<Host>>(&mut linker, |host| host)?;
        let component = Component::new(&engine, wasm)?;
        let host = Host::new(root, command_root, records)?;
        let mut store = Store::new(&engine, host);
        // An epoch-interrupted engine starts at deadline 0, which traps on the
        // first tick. Disarm start-up; each command arms its own timer.
        store.set_epoch_deadline(u64::MAX);
        let instance = Mush::instantiate(&mut store, &component, &linker)?;
        let mut args = vec![format!("--term={term}")];
        if let Some(principal) = principal {
            args.push(format!("--principal={principal}"));
        }
        if let Some(desktop) = desktop {
            args.push(format!("--desktop={}", desktop.id));
            args.push(format!("--desktop-root={}", root.display()));
            args.push(format!("--desktop-theme={}", desktop.theme));
        }
        let response = instance
            .call_start(&mut store, &args)?
            .map_err(anyhow::Error::msg)?;
        print!("{}", response.output);
        eprint!("{}", response.stderr);
        io::stdout().flush()?;
        Ok(Self {
            store,
            instance,
            engine,
            budget,
        })
    }

    /// Run one command line under the session deadline.
    ///
    /// A deadline trap is a result, not an internal error: the caller needs to
    /// know the command did not finish so it can report that instead of
    /// hanging or surfacing a Wasm trap the harness cannot act on.
    pub(crate) fn execute(&mut self, line: &str) -> Result<outcome::Executed> {
        // One epoch tick of headroom: the timer performs exactly one bump, so
        // that bump is what interrupts this command and nothing else.
        let timer = self
            .budget
            .map(|budget| deadline::Timer::arm(self.engine.clone(), budget));
        self.store
            .set_epoch_deadline(if timer.is_some() { 1 } else { u64::MAX });
        let result = self.instance.call_execute(&mut self.store, line);
        if let Some(timer) = &timer {
            timer.finish();
        }
        match result {
            Ok(response) => {
                let response = response.map_err(anyhow::Error::msg)?;
                Ok(outcome::Executed::done(
                    response.output,
                    response.stderr,
                    response.exit,
                    response.status,
                ))
            }
            Err(error) => {
                if !timer.as_ref().is_some_and(deadline::Timer::fired) {
                    return Err(error.into());
                }
                let budget = self.budget.unwrap_or_default();
                Ok(outcome::Executed {
                    output: String::new(),
                    stderr: deadline::timeout_hint(budget),
                    exit: true,
                    status: outcome::TIMEOUT_STATUS,
                    state: outcome::State::Timeout,
                })
            }
        }
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
    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
        println!("{}", usage());
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
    // A server subcommand owns the process until it is stopped. Starting one
    // from a caller that cannot interrupt it is the invocation that hangs an
    // agent harness forever, so it has to be asked for by name.
    let foreground = args.iter().any(|arg| arg == "--foreground");
    args.retain(|arg| arg != "--foreground");
    if mcp || serve {
        if args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
        {
            println!("{}", usage());
            return Ok(());
        }
        if !foreground && !io::stdin().is_terminal() {
            eprintln!("{}", server_hint(if mcp { "mcp" } else { "--serve" }));
            std::process::exit(2);
        }
    }
    // `mush sh` and the `-c` / `-lc` forms a terminal uses for a login shell.
    let mut shell_mode = false;
    if args.first().map(String::as_str) == Some("sh") {
        args.remove(0);
        shell_mode = true;
    }
    let mut shell_command = take_option(&mut args, "-c").or_else(|| take_option(&mut args, "-lc"));
    if shell_command.is_some() {
        shell_mode = true;
    }
    args.retain(|arg| arg != "-l" && arg != "-i");
    let config_path = take_option(&mut args, "--config")
        .map(|value| config::expand_home(&value))
        .unwrap_or_else(config::default_path);
    let config = config::load(&config_path)?;
    if args.first().map(String::as_str) == Some("desktop") {
        args.remove(0);
        return run_desktop(args);
    }
    if args.first().map(String::as_str) == Some("request") {
        args.remove(0);
        return run_request(&config, args);
    }
    if args.first().map(String::as_str) == Some("job") {
        args.remove(0);
        return run_job(&config, args);
    }
    if args.first().map(String::as_str) == Some("jobs") {
        args.remove(0);
        return run_jobs(&config, args);
    }
    if args.first().map(String::as_str) == Some("origin") {
        args.remove(0);
        return origin::run(args);
    }
    let explicit_root = args.iter().any(|arg| arg == "--root");
    let mut desktop_id = take_option(&mut args, "--desktop")
        .or_else(|| std::env::var("MUSH_DESKTOP").ok())
        .or_else(|| config.desktop.clone());
    // Nothing named a Desktop and nothing named a root: let the caller's origin
    // decide, so an agent started in a Delta worktree lands in its Desktop
    // instead of the default scratch root. The notice goes to stderr so it
    // never mixes with command output.
    if desktop_id.is_none() && !explicit_root {
        let cwd = std::env::current_dir()?;
        let origin = origin::resolve(&cwd);
        if let Some(id) = origin.desktop.clone() {
            eprintln!(
                "mush: origin desktop {id} ({}) from {}",
                origin.readiness.as_str(),
                cwd.display()
            );
            desktop_id = Some(id);
        }
    }
    let desktop = match desktop_id.as_deref() {
        Some(id) => {
            if explicit_root {
                anyhow::bail!("--desktop and --root are mutually exclusive");
            }
            Some(desktop::load(id)?)
        }
        None => None,
    };
    let root = if let Some(desktop) = &desktop {
        desktop.root.clone()
    } else {
        config::expand_home(
            &take_option(&mut args, "--root")
                .or_else(|| std::env::var("MUSH_ROOT").ok())
                .or_else(|| config.root.as_ref().map(|path| path.display().to_string()))
                .unwrap_or_else(|| "~/.local/share/fungos/vfs".into()),
        )
    };

    if args.first().map(String::as_str) == Some("run")
        && args.get(1).map(String::as_str) == Some("op")
    {
        args.drain(0..2);
        return run_operator(&root, args);
    }

    // Worktree and tree records are their own subcommand; they never start the
    // command engine.
    let state_root = take_option(&mut args, "--state-root")
        .or_else(|| std::env::var("MUSH_STATE_ROOT").ok())
        .map(|value| config::expand_home(&value))
        .unwrap_or_else(|| root.join(".fungos/records"));
    let worktree_book = take_option(&mut args, "--worktree-book")
        .or_else(|| std::env::var("MUSH_WORKTREE_BOOK").ok())
        .unwrap_or_else(|| "mush".into());
    let worktree_collection = take_option(&mut args, "--worktree-collection")
        .or_else(|| std::env::var("MUSH_WORKTREE_COLLECTION").ok())
        .unwrap_or_else(|| "worktree".into());
    if matches!(args.first().map(String::as_str), Some("worktree" | "tree")) {
        let command = args.remove(0);
        return worktree::run(
            &root,
            &command,
            args,
            state_root,
            worktree_book,
            worktree_collection,
        );
    }
    let wasm = take_option(&mut args, "--wasm")
        .or_else(|| std::env::var("MUSH_WASM").ok())
        .map(|value| config::expand_home(&value));
    let term = take_option(&mut args, "--term")
        .or_else(|| std::env::var("TERM").ok())
        .unwrap_or_else(|| "xterm-256color".into());
    let script = take_option(&mut args, "--script");
    let command = take_option(&mut args, "--command");
    // A human at a prompt is the clock; every other caller gets a deadline so
    // no command can park it indefinitely.
    let interactive = command.is_none() && script.is_none() && io::stdin().is_terminal();
    let budget =
        match take_option(&mut args, "--timeout").or_else(|| std::env::var("MUSH_TIMEOUT").ok()) {
            Some(value) => deadline::parse_budget(&value)?,
            None if interactive => None,
            None => Some(deadline::DEFAULT_TIMEOUT),
        };
    let history = take_option(&mut args, "--history")
        .or_else(|| std::env::var("MUSH_HISTORY").ok())
        .map(|value| config::expand_home(&value))
        // A stable per-user path, not `<root>/.fungos`: history that moves with
        // the VFS root is empty every time the root changes, which makes Ctrl-R
        // search nothing.
        .unwrap_or_else(|| config::expand_home("~/.local/state/fungos/mush/history"));
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
    // How long a daemon call waits before handing the caller a job id. The
    // adaptive default only applies when this is absent.
    let background_after = match take_option(&mut args, "--background-after")
        .or_else(|| std::env::var("MUSH_BACKGROUND_AFTER").ok())
    {
        Some(value) => deadline::parse_budget(&value)?,
        None => None,
    };
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
            desktop.as_ref(),
            budget,
        );
    }

    let bytes = match wasm {
        Some(wasm) => std::fs::read(&wasm)
            .with_context(|| format!("read Mush component {}", wasm.display()))?,
        None => EMBEDDED_MUSH.to_vec(),
    };
    let interactive_command_root = command_root.clone();
    let mut runtime = Runtime::open(
        &root,
        &bytes,
        &term,
        command_root,
        records,
        principal.as_deref(),
        desktop.as_ref(),
        budget,
    )?;

    if serve {
        let socket = socket
            .as_deref()
            .context("mush --serve needs --socket PATH or a config socket")?;
        let jobs = job::Jobs::start(runtime, background_after);
        return service::serve(socket, &jobs);
    }

    if mcp {
        let server = tiny_http::Server::http(&listen)
            .map_err(|error| anyhow::anyhow!("mcp listen {listen}: {error}"))?;
        if configured_token.is_none() {
            eprintln!("mush: mcp bearer token {mcp_token}");
        }
        let jobs = job::Jobs::start(runtime, background_after);
        return mcp_server::serve(server, &mcp_token, &jobs);
    }

    if shell_mode {
        let status = match shell_command {
            Some(line) => shell::run_line(&mut runtime, &root, &line)?,
            None => {
                let stdin = io::stdin();
                let mut status = 0;
                for line in stdin.lock().lines() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    status = shell::run_line(&mut runtime, &root, &line)?;
                }
                status
            }
        };
        return finish(status);
    }

    if let Some(command) = command {
        let mut status = 0;
        for line in command.lines() {
            let response = runtime.execute(line)?;
            print!("{}", response.output);
            eprint!("{}", response.stderr);
            status = response.status;
            if response.exit {
                return finish(status);
            }
        }
        return finish(status);
    }

    if let Some(script) = script {
        let response = runtime.execute(&format!("source {script}"))?;
        print!("{}", response.output);
        eprint!("{}", response.stderr);
        return finish(response.status);
    }

    // Nothing positional is valid here. Silently falling through to reading
    // stdin would turn a typo into a shell that waits forever.
    if !args.is_empty() {
        anyhow::bail!("unexpected arguments: {}", args.join(" "));
    }

    let status = if io::stdin().is_terminal() {
        run_interactive(
            &mut runtime,
            &history,
            &root,
            &interactive_command_root,
            config.prompt_command.as_deref(),
            desktop.as_ref().map(|desktop| desktop.mount.as_path()),
        )?
    } else {
        run_lines(&mut runtime)?
    };
    finish(status)
}

/// Ctrl-Z has nothing to suspend in Mush; explain that and carry on.
extern "C" fn on_suspend(_signal: libc::c_int) {
    let message = b"\nmush: Ctrl-Z does nothing here; commands run in-process, not as child jobs\n";
    // Async-signal-safe: write(2) on a static buffer, nothing else.
    unsafe {
        libc::write(
            libc::STDERR_FILENO,
            message.as_ptr() as *const libc::c_void,
            message.len(),
        );
    }
}

fn new_mcp_token() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// What a caller sees when it asks to start a server it cannot stop.
fn server_hint(name: &str) -> String {
    format!(
        "mush: {name} is a server and does not return\n\
         mush: hint: pass --foreground to start it anyway, or run it under a supervisor\n\
         mush: this refused instead of blocking your caller indefinitely"
    )
}

/// Exit with a command's own status so a wrapper sees what the command saw.
fn finish(status: i32) -> Result<()> {
    io::stdout().flush()?;
    std::process::exit(status);
}

fn usage() -> String {
    format!(
        "mush {}\n\n\
         Usage:\n\
         \x20 mush [OPTIONS]\n\
         \x20 mush doctor [OPTIONS]\n\
         \x20 mush run op [OPTIONS]\n\
         \x20 mush --serve --socket PATH --root DIR [--foreground] [OPTIONS]\n\
         \x20 mush mcp [--listen ADDR] [--foreground] [OPTIONS]\n\
         \x20 mush origin [--cwd PATH] [--json]\n\
         \x20 mush jobs [--json]\n\
         \x20 mush job ID [--json]\n\n\
         The Mush Spore is embedded; --wasm overrides it for development.\n\n\
         Options:\n\
         \x20 --root DIR                 VFS root (mutually exclusive with --desktop)\n\
         \x20 --desktop ID               select a Desktop registry entry\n\
         \x20 --wasm PATH                development Spore override\n\
         \x20 --command-root DIR         wasm command package root\n\
         \x20 --records-root DIR         Soil records root\n\
         \x20 --records-book BOOK        records book, e.g. personal/avery/marbles\n\
         \x20 --records-collection NAME  records collection name\n\
         \x20 --principal ID             signed principal label for this session\n\
         \x20 --socket PATH              Unix socket for --serve\n\
         \x20 --config PATH              config file (default: ~/.config/fungos/mush/config.toml)\n\
         \x20 --term TERM                terminal type passed to the Spore\n\
         \x20 --command LINE             execute one or more command lines\n\
         \x20 --script PATH              source a VFS script\n\
         \x20 --timeout SECONDS          interrupt a command after this long (0 disables)\n\
         \x20 --background-after SECONDS hand the caller a job id after this long (0 disables)\n\
         \x20 --foreground               allow a server subcommand to take the terminal\n\n\
         A non-interactive command runs with a {}s deadline unless --timeout says otherwise.\n\
         A daemon call that outlives its background threshold keeps running and returns a job id.\n\
         Servers refuse to start without --foreground when stdin is not a terminal.\n",
        env!("CARGO_PKG_VERSION"),
        deadline::DEFAULT_TIMEOUT.as_secs()
    )
}

fn run_desktop(mut args: Vec<String>) -> Result<()> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        anyhow::bail!("usage: mush desktop <list|show|create> [OPTIONS] [ID]");
    };
    match subcommand {
        "list" => {
            for desktop in desktop::list()? {
                println!(
                    "{}\t{}\t{}\tmount={}\ttheme={}",
                    desktop.id,
                    desktop.name,
                    desktop.root.display(),
                    desktop.mount.display(),
                    desktop.theme
                );
            }
        }
        "show" => {
            let id = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: mush desktop show ID"))?;
            let desktop = desktop::load(id)?;
            println!("id: {}", desktop.id);
            println!("name: {}", desktop.name);
            println!("root: {}", desktop.root.display());
            println!("mount: {}", desktop.mount.display());
            println!("theme: {}", desktop.theme);
            println!("projects: {}", desktop.projects.join(", "));
        }
        "create" => {
            let id = args
                .get(1)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "usage: mush desktop create ID [--name NAME] [--theme THEME] [--mount PATH]"
                    )
                })?
                .clone();
            let name = take_option(&mut args, "--name").unwrap_or_else(|| id.clone());
            let theme = take_option(&mut args, "--theme").unwrap_or_else(|| "deep".into());
            let mount = take_option(&mut args, "--mount");
            let desktop = desktop::create(&id, &name, &theme, mount.as_deref())?;
            println!("{}", desktop.id);
        }
        "add-project" => {
            let desktop_id = args
                .get(1)
                .ok_or_else(|| {
                    anyhow::anyhow!("usage: mush desktop add-project DESKTOP PROJECT --from PATH")
                })?
                .clone();
            let project = args
                .get(2)
                .ok_or_else(|| {
                    anyhow::anyhow!("usage: mush desktop add-project DESKTOP PROJECT --from PATH")
                })?
                .clone();
            let from = take_option(&mut args, "--from")
                .or_else(|| args.get(3).cloned())
                .ok_or_else(|| {
                    anyhow::anyhow!("usage: mush desktop add-project DESKTOP PROJECT --from PATH")
                })?;
            let (files, bytes) = project::attach(&desktop_id, &project, &from)?;
            println!("projects/{project} ({files} files, {bytes} bytes)");
        }
        other => anyhow::bail!("unknown desktop subcommand {other:?}"),
    }
    Ok(())
}

fn run_request(config: &config::Config, mut args: Vec<String>) -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let socket = service_socket(config, &mut args)?;
    let json = args.iter().any(|arg| arg == "--json");
    args.retain(|arg| arg != "--json");
    // The client needs its own bound: a wedged daemon must not hang the
    // caller either, and the deadline here is what the agent harness sees.
    let budget =
        match take_option(&mut args, "--timeout").or_else(|| std::env::var("MUSH_TIMEOUT").ok()) {
            Some(value) => deadline::parse_budget(&value)?,
            None => Some(deadline::DEFAULT_TIMEOUT),
        };
    if args.is_empty() {
        anyhow::bail!(
            "usage: mush request [--socket PATH] [--json] [--timeout SECONDS] COMMAND..."
        );
    }
    let command = args.join(" ");
    let background_after = take_option(&mut args, "--background-after")
        .or_else(|| std::env::var("MUSH_BACKGROUND_AFTER").ok());
    let background_after_secs = match background_after {
        Some(value) => deadline::parse_budget(&value)?.map(|budget| budget.as_secs()),
        None => None,
    };
    let mut request = serde_json::json!({"command": command});
    if let Some(seconds) = background_after_secs {
        request["background_after_secs"] = serde_json::json!(seconds);
    }
    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("connect Mush service {}", socket.display()))?;
    if let Some(budget) = budget {
        stream.set_read_timeout(Some(budget))?;
        stream.set_write_timeout(Some(budget))?;
    }
    stream.write_all(serde_json::to_string(&request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut line = String::new();
    let read = BufReader::new(stream).read_line(&mut line);
    if let Err(error) = read {
        if matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ) {
            eprintln!("{}", deadline::timeout_hint(budget.unwrap_or_default()));
            std::process::exit(outcome::TIMEOUT_STATUS);
        }
        return Err(error.into());
    }
    let response: serde_json::Value = serde_json::from_str(&line).context("parse Mush response")?;
    let background =
        response.get("state").and_then(serde_json::Value::as_str) == Some("background");
    if json {
        print!("{line}");
        if !line.ends_with('\n') {
            println!();
        }
    } else if background {
        // The work is still alive; this is the receipt, on stdout where a
        // caller that only reads output will see it.
        let hint = response
            .get("hint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("command still running");
        println!("{hint}");
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
    if background {
        return Ok(());
    }
    let status = response
        .get("status")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0) as i32;
    if status != 0 {
        std::process::exit(status);
    }
    Ok(())
}

/// Resolve the daemon socket from flags, environment, or config.
fn service_socket(config: &config::Config, args: &mut Vec<String>) -> Result<PathBuf> {
    take_option(args, "--socket")
        .or_else(|| std::env::var("MUSH_SOCKET").ok())
        .or_else(|| {
            config
                .socket
                .as_ref()
                .map(|path| path.display().to_string())
        })
        .map(|value| config::expand_home(&value))
        .context("needs --socket PATH or a config socket")
}

/// One request, one response line, bounded so a wedged daemon cannot park us.
fn service_query(socket: &Path, request: &serde_json::Value) -> Result<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connect Mush service {}", socket.display()))?;
    stream.set_read_timeout(Some(deadline::DEFAULT_TIMEOUT))?;
    stream.write_all(serde_json::to_string(request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).context("parse Mush response")
}

fn run_jobs(config: &config::Config, mut args: Vec<String>) -> Result<()> {
    let socket = service_socket(config, &mut args)?;
    let reply = service_query(&socket, &serde_json::json!({"kind": "jobs"}))?;
    if args.iter().any(|arg| arg == "--json") {
        println!("{}", serde_json::to_string_pretty(&reply)?);
        return Ok(());
    }
    let empty = Vec::new();
    let jobs = reply
        .get("jobs")
        .and_then(serde_json::Value::as_array)
        .unwrap_or(&empty);
    if jobs.is_empty() {
        println!("no jobs");
        return Ok(());
    }
    for job in jobs {
        println!(
            "{}\t{}\t{}",
            job.get("job")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-"),
            job.get("state")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-"),
            job.get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-"),
        );
    }
    Ok(())
}

fn run_job(config: &config::Config, mut args: Vec<String>) -> Result<()> {
    let socket = service_socket(config, &mut args)?;
    let json = args.iter().any(|arg| arg == "--json");
    args.retain(|arg| arg != "--json");
    let id = args
        .first()
        .cloned()
        .context("usage: mush job ID [--json]")?;
    let reply = service_query(&socket, &serde_json::json!({"kind": "job", "job": id}))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reply)?);
        return Ok(());
    }
    if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        anyhow::bail!(
            "{}",
            reply
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown job")
        );
    }
    let state = reply
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    if let Some(stdout) = reply.get("stdout").and_then(serde_json::Value::as_str) {
        print!("{stdout}");
    }
    if let Some(stderr) = reply.get("stderr").and_then(serde_json::Value::as_str) {
        eprint!("{stderr}");
    }
    io::stdout().flush()?;
    if state == "running" {
        println!("state: running");
        if let Some(hint) = reply.get("hint").and_then(serde_json::Value::as_str) {
            println!("{hint}");
        }
        return Ok(());
    }
    let status = reply
        .get("status")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0) as i32;
    if status != 0 {
        return finish(status);
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
    desktop: Option<&desktop::Desktop>,
    budget: Option<std::time::Duration>,
) -> Result<()> {
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let uid = unsafe { libc::geteuid() };
    println!("mush {}", env!("CARGO_PKG_VERSION"));
    println!("user: {user} (uid {uid})");
    println!("config: {}", config_path.display());
    match budget {
        Some(budget) => println!("command deadline: {}s [enabled]", budget.as_secs_f64()),
        None => println!("command deadline: disabled [interactive]"),
    }
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
    match desktop {
        Some(desktop) => println!("desktop: {} ({})", desktop.id, desktop.name),
        None => println!("desktop: disabled"),
    }
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

fn run_interactive(
    runtime: &mut Runtime,
    history: &Path,
    root: &Path,
    command_root: &Path,
    prompt_command: Option<&str>,
    mount: Option<&Path>,
) -> Result<i32> {
    // Mush runs every command in-process, so there is no child to suspend or
    // resume. Letting Ctrl-Z land would stop the shell while the terminal is
    // still in raw mode, which corrupts the session on `fg` and has nothing to
    // resume anyway. Report the boundary instead of breaking the terminal.
    unsafe { libc::signal(libc::SIGTSTP, on_suspend as *const () as libc::sighandler_t) };
    let editor_config = rustyline::Config::builder()
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut editor = Editor::<complete::MushHelper, FileHistory>::with_config(editor_config)?;
    let helper = complete::MushHelper::new(root.to_path_buf(), command_root);
    let cwd = helper.cwd_handle();
    editor.set_helper(Some(helper));
    let _ = editor.load_history(history);
    let mut status = 0;
    let mut elapsed = std::time::Duration::ZERO;
    refresh_cwd(runtime, &cwd);
    loop {
        let prompt = match prompt_command {
            Some(command) => match prompt_via_command(command, status, elapsed, &cwd, mount) {
                Some(text) => text,
                // A broken prompt command must not cost the session its prompt.
                None => runtime.prompt()?,
            },
            None => runtime.prompt()?,
        };
        let started = std::time::Instant::now();
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
                status = response.status;
                elapsed = started.elapsed();
                if changes_directory(line) {
                    refresh_cwd(runtime, &cwd);
                }
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
    Ok(status)
}

/// Keep the completer's idea of the shell's cwd in step with the Spore's.
///
/// `pwd` is the only way the host can ask, and running it resets `$?`, so this
/// happens at start-up and after `cd` rather than before every prompt.
fn refresh_cwd(runtime: &mut Runtime, cwd: &std::sync::Arc<std::sync::Mutex<String>>) {
    let Ok(executed) = runtime.execute("pwd") else {
        return;
    };
    let value = executed.output.trim();
    if value.is_empty() {
        return;
    }
    if let Ok(mut guard) = cwd.lock() {
        *guard = value.to_string();
    }
}

fn changes_directory(line: &str) -> bool {
    line.split_whitespace().next() == Some("cd")
}

/// Run the configured prompt command and use its stdout as the prompt.
///
/// This is how a theme reaches the shell: the host owns process spawning, so a
/// prompt renderer never has to enter the Spore. Starship needs no adapter:
///
/// ```text
/// prompt_command = "starship prompt --status $MUSH_STATUS --cmd-duration $MUSH_DURATION_MS"
/// ```
fn prompt_via_command(
    command: &str,
    status: i32,
    elapsed: std::time::Duration,
    cwd: &std::sync::Arc<std::sync::Mutex<String>>,
    mount: Option<&Path>,
) -> Option<String> {
    let current = cwd.lock().map(|cwd| cwd.clone()).unwrap_or_default();
    // A prompt renderer that inspects a directory -- for a Git branch, say --
    // needs the projected host path, not the VFS path it cannot open.
    let host_cwd = mount.and_then(|mount| host_cwd_for(mount, &current));
    let mut process = std::process::Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .env("MUSH_STATUS", status.to_string())
        .env("MUSH_DURATION_MS", elapsed.as_millis().to_string())
        .env("MUSH_CWD", &current)
        .env("STARSHIP_SHELL", "mush")
        .stdin(std::process::Stdio::null());
    if let Some(host_cwd) = &host_cwd {
        process.current_dir(host_cwd);
        process.env("MUSH_HOST_CWD", host_cwd);
    }
    let output = process.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Where a VFS path lands on this device, when the Desktop is projected here.
fn host_cwd_for(mount: &Path, vfs_cwd: &str) -> Option<std::path::PathBuf> {
    let relative = vfs_cwd.trim_start_matches('/');
    let path = if relative.is_empty() {
        mount.to_path_buf()
    } else {
        mount.join(relative)
    };
    path.is_dir().then_some(path)
}

fn run_lines(runtime: &mut Runtime) -> Result<i32> {
    let stdin = io::stdin();
    let mut status = 0;
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
        status = response.status;
        if response.exit {
            break;
        }
    }
    Ok(status)
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
