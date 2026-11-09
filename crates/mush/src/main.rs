//! Native host for the Mush Spore.
//!
//! The component owns shell state and command semantics. This process owns the
//! VFS root, the `soil:vfs` / `soil:index` / `soil:vfs-mut` grants, and the
//! line-oriented terminal.

mod gateway;
mod mcp_gateway;
mod overlay;
mod worktree;

use std::{
    collections::BTreeMap,
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use rustyline::{DefaultEditor, error::ReadlineError};
use serde::{Deserialize, Serialize};
use soil::command::command as command_abi;
use soil_command::{CommandRegistry, CommandRunner, RecordsBinding, RunRequest};
use soil_inference::{GeodeResolver, SoilConfig};
use soil_vfs_host::{VfsHost, index, vfs, vfs_mut};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({ path: "../../spores/mush/wit", world: "mush" });

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

struct Runtime {
    store: Store<Host>,
    instance: Mush,
}

impl Runtime {
    fn open(
        root: &std::path::Path,
        wasm: &[u8],
        term: &str,
        command_root: PathBuf,
        records: Option<RecordsBinding>,
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

/// A login shell is invoked as `-mush` (argv[0] with a leading dash). That is
/// the only signal a shell gets that nothing else sourced a profile for it.
fn login_invocation() -> bool {
    std::env::args_os()
        .next()
        .and_then(|arg0| {
            Path::new(&arg0)
                .file_name()
                .map(|name| name.to_string_lossy().starts_with('-'))
        })
        .unwrap_or(false)
}

fn push_dir(dirs: &mut Vec<String>, dir: String) {
    if Path::new(&dir).is_dir() && !dirs.iter().any(|existing| existing == &dir) {
        dirs.push(dir);
    }
}

/// The PATH a login shell would have had. A user toolchain wins over the system
/// one, duplicates collapse, and only directories that exist are kept.
fn login_path(existing: &str, home: &str) -> String {
    let mut dirs = Vec::new();
    for dir in [
        format!("{home}/.cargo/bin"),
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/sbin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/local/sbin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
        "/usr/sbin".to_string(),
        "/sbin".to_string(),
    ] {
        push_dir(&mut dirs, dir);
    }
    for dir in existing.split(':').filter(|dir| !dir.is_empty()) {
        push_dir(&mut dirs, dir.to_string());
    }
    dirs.join(":")
}

/// Login shells have no profile to source, so they build the environment the
/// host bridge will inherit. `run host` copies PATH from this process, which is
/// why setting it here is enough for `cargo` and `git` to resolve.
fn apply_login_env() {
    let home = std::env::var("HOME").unwrap_or_default();
    let existing = std::env::var("PATH").unwrap_or_default();
    let path = login_path(&existing, &home);
    // Single-threaded: no runtime, no thread, nothing else reads the
    // environment yet.
    unsafe {
        if !path.is_empty() {
            std::env::set_var("PATH", path);
        }
        std::env::set_var("MUSH_LOGIN", "1");
    }
}

/// `-l`, `-i`, `-lc`, `-lic`: a flag that carries a command is the one that
/// decides whether we are a shell or a REPL.
fn has_shell_command_flag(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg.starts_with('-')
            && arg.len() > 1
            && arg[1..].chars().all(|c| matches!(c, 'l' | 'i' | 'c'))
            && arg.contains('c')
    })
}

/// A flag that asks for login semantics: `-l`, `-li`, `-lc`, `-lic`.
fn is_login_flag(arg: &str) -> bool {
    arg.starts_with('-')
        && arg.len() > 1
        && arg[1..].chars().all(|c| matches!(c, 'l' | 'i' | 'c'))
        && arg.contains('l')
}

/// A script that starts with `#!` keeps everything after that line.
fn strip_shebang(body: &str) -> &str {
    match body.strip_prefix("#!") {
        Some(rest) => rest.split_once('\n').map(|(_, tail)| tail).unwrap_or(""),
        None => body,
    }
}

/// Run a host file line by line. Scripts are host files: the kernel resolves the
/// path, so the Spore's `source` (which reads the VFS) is not the right tool.
fn run_script(runtime: &mut Runtime, path: &Path) -> Result<()> {
    let body =
        std::fs::read_to_string(path).with_context(|| format!("read script {}", path.display()))?;
    for line in strip_shebang(&body).lines() {
        let response = runtime.execute(line)?;
        print!("{}", response.output);
        if response.exit {
            break;
        }
    }
    Ok(())
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    // A login shell is invoked as `-mush` (argv[0] with a leading dash) or with
    // `-l`. Nothing sources /etc/profile for it, so it builds its own
    // environment before anything else runs.
    let login = login_invocation() || args.iter().any(|arg| is_login_flag(arg));
    if login {
        apply_login_env();
    }
    let root = take_option(&mut args, "--root")
        .or_else(|| std::env::var("MUSH_ROOT").ok())
        .unwrap_or_else(|| "~/.local/share/fungos/vfs".into());
    let root = expand_home(&root);

    // `mush sh` (and a bare `mush -c CMD`) is the POSIX-ish entry point: one
    // command line, VFS commands through the Spore, allowlisted host programs
    // through the overlay bridge, and session state that outlives the process
    // because a terminal spawns a fresh shell for every tool call.
    let shell = if args.first().map(String::as_str) == Some("sh") {
        args.remove(0);
        Some(parse_shell_args(&mut args)?)
    } else if args.first().map(String::as_str) == Some("-c") || has_shell_command_flag(&args) {
        Some(parse_shell_args(&mut args)?)
    } else {
        None
    };

    // A pure `-l` / `-i` flag carries no command, so what follows it may still be
    // a script path. Drop the flags, keep the path.
    while let Some(flag) = args.first() {
        let is_flag = flag.starts_with('-')
            && flag.len() > 1
            && flag[1..].chars().all(|c| matches!(c, 'l' | 'i'));
        if !is_flag {
            break;
        }
        args.remove(0);
    }

    if args.first().map(String::as_str) == Some("run")
        && args.get(1).map(String::as_str) == Some("host")
    {
        args.drain(0..2);
        return run_host(&root, args);
    }

    if args.first().map(String::as_str) == Some("run")
        && args.get(1).map(String::as_str) == Some("op")
    {
        args.drain(0..2);
        return run_operator(&root, args);
    }

    let state_root = take_option(&mut args, "--state-root")
        .or_else(|| std::env::var("MUSH_STATE_ROOT").ok())
        .map(|value| expand_home(&value))
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
    let command_root = take_option(&mut args, "--command-root")
        .or_else(|| std::env::var("FUNGOS_COMMAND_ROOT").ok())
        .map(|value| expand_home(&value))
        .unwrap_or_else(|| expand_home("~/.local/share/fungos/commands"));
    let records_root = take_option(&mut args, "--records-root")
        .or_else(|| std::env::var("FUNGOS_RECORDS_ROOT").ok())
        .map(|value| expand_home(&value));
    let records_book = take_option(&mut args, "--records-book")
        .or_else(|| std::env::var("FUNGOS_RECORDS_BOOK").ok());
    let records_collection = take_option(&mut args, "--records-collection")
        .or_else(|| std::env::var("FUNGOS_RECORDS_COLLECTION").ok());
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
    if let Some(shell) = shell {
        return run_shell(&root, &bytes, &term, command_root, records, shell);
    }
    let mut runtime = Runtime::open(&root, &bytes, &term, command_root, records)?;

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

    // `mush path/to/script.mush`, which is what `#!/usr/local/bin/mush` becomes
    // once the kernel hands us the file. The shebang line is stripped here, not
    // in the Spore: the Spore has no comment marker to skip it with.
    if let Some(first) = args.first() {
        let path = PathBuf::from(first);
        if path.is_file() {
            return run_script(&mut runtime, &path);
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Connector {
    First,
    Always,
    And,
    Or,
}

/// Commands the Spore owns. Anything else has to be an allowlisted host program.
const VFS_COMMANDS: &[&str] = &[
    "help", "pwd", "cd", "ls", "mkdir", "rmdir", "cat", "write", "append", "touch", "head", "tail",
    "grep", "wc", "stat", "tree", "find", "rm", "mv", "cp", "search", "source", "echo", "export",
    "unset", "env", "set", "exit",
];

#[derive(Debug, Default)]
struct ShellArgs {
    /// The `-c` command line. None means read stdin.
    command: Option<String>,
    session: Option<String>,
}

/// Accept what a terminal actually passes to a shell (`-l`, `-i`, `-lc`,
/// `-lic`) and ignore the parts that only matter to a login shell.
fn parse_shell_args(args: &mut Vec<String>) -> Result<ShellArgs> {
    let mut command = None;
    loop {
        let Some(flag) = args.first().cloned() else {
            break;
        };
        let is_flag = flag.starts_with('-')
            && flag.len() > 1
            && flag[1..].chars().all(|c| matches!(c, 'l' | 'i' | 'c'));
        if !is_flag {
            break;
        }
        args.remove(0);
        if flag.contains('c') {
            command = Some(args.first().cloned().context("mush: -c needs a command")?);
            args.remove(0);
        }
    }
    Ok(ShellArgs {
        command,
        session: take_option(args, "--session"),
    })
}

/// Shell state that has to survive a process: a terminal spawns a new shell for
/// every tool call, so `cd` and `export` would otherwise reset each time.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Session {
    #[serde(default = "root_pwd")]
    pwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Identifies the file; not content.
    #[serde(skip)]
    key: String,
}

fn root_pwd() -> String {
    "/".into()
}

fn sanitize_key(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "default".into()
    } else {
        cleaned
    }
}

/// One session per agent when the runtime names it, otherwise one per VFS root.
fn session_key(explicit: Option<String>, root: &Path) -> String {
    explicit
        .filter(|k| !k.is_empty())
        .or_else(|| std::env::var("MUSH_SESSION").ok().filter(|k| !k.is_empty()))
        .or_else(|| {
            std::env::var("BOB_AGENT_KEY")
                .ok()
                .filter(|k| !k.is_empty())
        })
        .unwrap_or_else(|| {
            let mut h: u64 = 1469598103934665603;
            for byte in root.to_string_lossy().as_bytes() {
                h ^= *byte as u64;
                h = h.wrapping_mul(1099511628211);
            }
            format!("root-{h:x}")
        })
}

/// Quote for the Spore's line parser. Values it cannot round-trip are dropped
/// by `plain_word` rather than written back corrupted.
fn quote_word(value: &str) -> String {
    if plain_word(value) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', ""))
    }
}

fn plain_word(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+,=".contains(c))
}

impl Session {
    fn path(root: &Path, key: &str) -> PathBuf {
        root.join(".fungos/sessions")
            .join(format!("{}.json", sanitize_key(key)))
    }

    fn load(root: &Path, key: &str) -> Session {
        let mut session = std::fs::read(Self::path(root, key))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Session>(&bytes).ok())
            .unwrap_or_default();
        if session.pwd.is_empty() {
            session.pwd = root_pwd();
        }
        session.key = key.to_string();
        session
    }

    /// Fail-open: session state is a convenience, never a build dependency.
    fn save(&self, root: &Path) {
        let path = Self::path(root, &self.key);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }

    fn restore(&self, runtime: &mut Runtime) -> Result<()> {
        if self.pwd != root_pwd() {
            let response = runtime.execute(&format!("cd {}", quote_word(&self.pwd)))?;
            print!("{}", response.output);
        }
        for (key, value) in &self.env {
            let _ = runtime.execute(&format!("export {key}={}", quote_word(value)));
        }
        Ok(())
    }

    fn capture(&mut self, runtime: &mut Runtime) -> Result<()> {
        let pwd = runtime.execute("pwd")?;
        if let Some(line) = pwd.output.lines().next() {
            let line = line.trim();
            if line.starts_with('/') {
                self.pwd = line.to_string();
            }
        }
        let env = runtime.execute("env")?;
        let mut map = BTreeMap::new();
        for line in env.output.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            if matches!(key, "PWD" | "OLDPWD" | "SHLVL" | "_") {
                continue;
            }
            if !plain_word(value) {
                continue;
            }
            map.insert(key.to_string(), value.to_string());
        }
        self.env = map;
        Ok(())
    }
}

/// Split a command line on `;`, `&&`, and `||`, leaving quoted operators alone.
fn split_commands(line: &str) -> Vec<(Connector, String)> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut connector = Connector::First;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                buf.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    buf.push(c);
                }
                ';' => {
                    out.push((connector, buf.trim().to_string()));
                    buf.clear();
                    connector = Connector::Always;
                }
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    out.push((connector, buf.trim().to_string()));
                    buf.clear();
                    connector = Connector::And;
                }
                '|' if chars.peek() == Some(&'|') => {
                    chars.next();
                    out.push((connector, buf.trim().to_string()));
                    buf.clear();
                    connector = Connector::Or;
                }
                _ => buf.push(c),
            },
        }
    }
    if !buf.trim().is_empty() || out.is_empty() {
        out.push((connector, buf.trim().to_string()));
    }
    out
}

/// Split one simple command into argv, resolving quotes and backslashes.
fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut buf = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    buf.push(c);
                }
                started = true;
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        buf.push(next);
                        started = true;
                    }
                }
                c if c.is_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut buf));
                        started = false;
                    }
                }
                c => {
                    buf.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        words.push(buf);
    }
    words
}

fn execute_one(runtime: &mut Runtime, root: &Path, session: &Session, line: &str) -> Result<i32> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(0);
    }
    let words = split_words(line);
    let Some(program) = words.first().cloned() else {
        return Ok(0);
    };
    if VFS_COMMANDS.contains(&program.as_str()) {
        let response = runtime.execute(line)?;
        print!("{}", response.output);
        io::stdout().flush()?;
        return Ok(0);
    }
    if host_allowed(&program) {
        if line.contains('>') {
            anyhow::bail!("mush: {program}: redirection is only supported for VFS commands");
        }
        let mut args = vec![
            "--cwd".to_string(),
            session.pwd.trim_start_matches('/').to_string(),
        ];
        args.push("--".into());
        args.extend(words);
        return run_host_status(root, args);
    }
    eprintln!("mush: {program}: not a VFS command and not an allowlisted host program");
    Ok(127)
}

fn execute_line(runtime: &mut Runtime, root: &Path, session: &Session, line: &str) -> Result<i32> {
    let mut status = 0;
    for (connector, command) in split_commands(line) {
        let should_run = match connector {
            Connector::First | Connector::Always => true,
            Connector::And => status == 0,
            Connector::Or => status != 0,
        };
        if !should_run || command.is_empty() {
            continue;
        }
        status = execute_one(runtime, root, session, &command)?;
    }
    Ok(status)
}

fn run_shell(
    root: &Path,
    wasm: &[u8],
    term: &str,
    command_root: PathBuf,
    records: Option<RecordsBinding>,
    args: ShellArgs,
) -> Result<()> {
    let key = session_key(args.session, root);
    let mut session = Session::load(root, &key);
    let mut runtime = Runtime::open(root, wasm, term, command_root, records)?;
    session.restore(&mut runtime)?;
    let status = match args.command {
        Some(command) => execute_line(&mut runtime, root, &session, &command)?,
        None => {
            let stdin = io::stdin();
            let mut status = 0;
            for line in stdin.lock().lines() {
                let line = line?;
                if line.trim() == "exit" {
                    break;
                }
                status = execute_line(&mut runtime, root, &session, &line)?;
            }
            status
        }
    };
    session.capture(&mut runtime)?;
    session.save(root);
    if status != 0 {
        std::process::exit(status);
    }
    Ok(())
}

/// Host commands allowed to run against a materialized VFS overlay. This is a
/// policy boundary, not a sandbox: an allowed program still runs as the user
/// and can read their home. Keep it to build tools, and never add a shell.
const HOST_ALLOWLIST: &[&str] = &[
    "cargo",
    "rustc",
    "cc",
    "clang",
    "clang++",
    "c++",
    "git",
    "make",
    "cmake",
    "ninja",
    "pkg-config",
];

/// A bare program name only. A path like `./cargo` would run code from the
/// overlay itself, which is exactly what the allowlist is meant to prevent.
fn host_allowed(program: &str) -> bool {
    !program.contains('/') && HOST_ALLOWLIST.contains(&program)
}

fn run_host(root: &Path, args: Vec<String>) -> Result<()> {
    let status = run_host_status(root, args)?;
    if status != 0 {
        std::process::exit(status);
    }
    Ok(())
}

/// The status-returning form, so a shell can chain `&&` and `||` without
/// exiting the process.
fn run_host_status(root: &Path, mut args: Vec<String>) -> Result<i32> {
    let passthrough = args
        .iter()
        .position(|arg| arg == "--")
        .map(|index| args.split_off(index + 1));
    if args.last().map(String::as_str) == Some("--") {
        args.pop();
    }
    let keep = args.iter().any(|arg| arg == "--keep");
    let no_apply = args.iter().any(|arg| arg == "--no-apply");
    args.retain(|arg| arg != "--keep" && arg != "--no-apply");
    let cwd = take_option(&mut args, "--cwd").unwrap_or_default();
    let mut extra_env = Vec::new();
    while let Some(var) = take_option(&mut args, "--allow-env") {
        extra_env.push(var);
    }
    let cmd = passthrough.unwrap_or_default();
    if cmd.is_empty() {
        anyhow::bail!("mush run host: no command (use `mush run host -- cargo build`)");
    }
    if !host_allowed(&cmd[0]) {
        anyhow::bail!(
            "mush run host: `{}` is not allowlisted (allowed: {})",
            cmd[0],
            HOST_ALLOWLIST.join(", ")
        );
    }

    let overlay = overlay::Overlay::materialize(root)?;
    let workspace = overlay.path().to_path_buf();
    let dir = if cwd.is_empty() {
        workspace.clone()
    } else {
        workspace.join(&cwd)
    };
    if !dir.starts_with(&workspace) {
        anyhow::bail!("mush run host: --cwd escapes the overlay");
    }
    if !dir.is_dir() {
        anyhow::bail!("mush run host: --cwd {} is not a directory", cwd);
    }

    let mut command = Command::new(&cmd[0]);
    command.args(&cmd[1..]).current_dir(&dir);
    // Start empty: a host build needs its toolchain, not the agent's tokens.
    // Anything beyond this list is opt-in via --allow-env.
    command.env_clear();
    for var in [
        "PATH",
        "HOME",
        "USER",
        "LANG",
        "TERM",
        "TMPDIR",
        "RUSTUP_HOME",
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        "SCCACHE_CONF",
        "RUSTC_WRAPPER",
        "SCCACHE_SKIP_CACHE_CHECK",
        "BOB_AGENT_KEY",
    ] {
        if let Ok(value) = std::env::var(var) {
            command.env(var, value);
        }
    }
    for var in &extra_env {
        if let Ok(value) = std::env::var(var) {
            command.env(var, value);
        }
    }
    command.env("MUSH_ROOT", root);
    command.env("MUSH_OVERLAY", &workspace);
    eprintln!("mush: overlay {}", workspace.display());

    let status = command.status().context("run host command")?;
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
    Ok(status.code().unwrap_or(1))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowlist_is_a_bare_program_name() {
        assert!(host_allowed("cargo"));
        assert!(host_allowed("clang++"));
        assert!(host_allowed("pkg-config"));
        assert!(!host_allowed("sh"));
        assert!(!host_allowed("/bin/sh"));
        assert!(!host_allowed("./cargo"));
        assert!(!host_allowed("cargo/../cargo"));
    }

    #[test]
    fn shell_splits_commands_without_breaking_quotes() {
        let parts = split_commands("cd /a && echo 'x && y'; false || true");
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], (Connector::First, "cd /a".to_string()));
        assert_eq!(parts[1], (Connector::And, "echo 'x && y'".to_string()));
        assert_eq!(parts[2], (Connector::Always, "false".to_string()));
        assert_eq!(parts[3], (Connector::Or, "true".to_string()));
    }

    #[test]
    fn shell_words_resolve_quotes_and_backslashes() {
        assert_eq!(
            split_words("cargo build --features 'a b'"),
            vec!["cargo", "build", "--features", "a b"]
        );
        assert_eq!(split_words(r#"echo "x y" z"#), vec!["echo", "x y", "z"]);
        assert_eq!(split_words(r"echo a\ b"), vec!["echo", "a b"]);
    }

    #[test]
    fn session_round_trips_and_keys_are_sanitized() {
        let mut session = Session::default();
        session.pwd = "/projects/app".into();
        session.env.insert("FOO".into(), "bar".into());
        session.key = "agent/1".into();
        let bytes = serde_json::to_vec(&session).unwrap();
        let back: Session = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.pwd, "/projects/app");
        assert_eq!(back.env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(sanitize_key("agent/1"), "agent-1");
        assert_eq!(sanitize_key(""), "default");
        assert_eq!(quote_word("plain"), "plain");
        assert_eq!(quote_word("a b"), "'a b'");
        assert!(!plain_word("a b"));
    }

    #[test]
    fn login_path_prefers_existing_dirs_and_dedups() {
        let path = login_path("/usr/bin:/usr/bin:/definitely-not-here", "/home/u");
        assert_eq!(path.matches("/usr/bin").count(), 1, "{path}");
        assert!(!path.contains("/definitely-not-here"));
        assert!(
            path.split(':').all(|dir| Path::new(dir).is_dir()),
            "every entry must exist: {path}"
        );
    }

    #[test]
    fn shell_command_flag_distinguishes_login_from_command() {
        let args = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert!(has_shell_command_flag(&args(&["-c", "pwd"])));
        assert!(has_shell_command_flag(&args(&["-lc", "pwd"])));
        assert!(has_shell_command_flag(&args(&["-lic", "pwd"])));
        assert!(!has_shell_command_flag(&args(&["-l"])));
        assert!(!has_shell_command_flag(&args(&["-i"])));
        assert!(!has_shell_command_flag(&args(&["--command", "pwd"])));
    }

    #[test]
    fn shebang_line_is_not_executed() {
        assert_eq!(strip_shebang("#!/usr/local/bin/mush\npwd\n"), "pwd\n");
        assert_eq!(strip_shebang("pwd\n"), "pwd\n");
        assert_eq!(strip_shebang("#!/bin/mush"), "");
    }

    #[test]
    fn login_flags_are_recognised_without_swallowing_commands() {
        assert!(is_login_flag("-l"));
        assert!(is_login_flag("-lc"));
        assert!(is_login_flag("-lic"));
        assert!(!is_login_flag("-c"));
        assert!(!is_login_flag("-i"));
        assert!(!is_login_flag("--command"));
    }
}
