//! Signed-command registry and wasm CLI runner.
//!
//! The registry is a virtual PATH: names resolve to installed command
//! directories, never to host binaries. The runner uses WASI stdio and links
//! only the host capabilities configured for that command.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{
    WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView,
    p2::{
        bindings::sync::Command,
        pipe::{MemoryInputPipe, MemoryOutputPipe},
    },
};

use soil_host::records_host::RecordsBookHost;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    command: CommandMeta,
    #[serde(default)]
    limits: Limits,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandMeta {
    name: String,
    version: String,
    summary: String,
    module: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    #[serde(default = "default_stdout")]
    max_stdout_bytes: usize,
    #[serde(default = "default_stderr")]
    max_stderr_bytes: usize,
    #[serde(default = "default_memory")]
    max_memory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout_ms: default_timeout(),
            max_stdout_bytes: default_stdout(),
            max_stderr_bytes: default_stderr(),
            max_memory_bytes: default_memory(),
        }
    }
}

fn default_timeout() -> u64 {
    30_000
}
fn default_stdout() -> usize {
    1024 * 1024
}
fn default_stderr() -> usize {
    1024 * 1024
}
fn default_memory() -> usize {
    256 * 1024 * 1024
}

#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub version: String,
    pub summary: String,
    /// The package directory that declares it.
    pub package: PathBuf,
    /// Capabilities the package declares for this command. A command is run by a
    /// host that links some of them and not others, so the registry checks this
    /// before it promises a program.
    pub requires: Vec<String>,
    pub module: PathBuf,
    pub timeout_ms: u64,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_memory_bytes: usize,
}

/// A candidate that did not become a command, and why.
///
/// The registry keeps answering with everything else. One package that names a
/// module it does not carry, or one name claimed twice, must not empty a shell's
/// PATH; the refusal is what keeps the loss visible instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub name: String,
    pub reason: String,
}

pub const RUNNER_CAPABILITIES: &[&str] = &["soil:records"];

#[derive(Debug, Clone)]
pub struct CommandRegistry {
    roots: Vec<PathBuf>,
    commands: BTreeMap<String, CommandEntry>,
    refused: Vec<Refusal>,
}

impl CommandRegistry {
    /// An empty registry for hosts that start before any command is installed.
    pub fn empty(root: impl AsRef<Path>) -> Self {
        Self::over(vec![root.as_ref().to_path_buf()])
    }

    /// Scan one root.
    pub fn scan(root: impl AsRef<Path>) -> Result<Self> {
        Self::scan_roots(&[root.as_ref().to_path_buf()])
    }

    /// Scan roots in order, most specific first.
    ///
    /// The first package to claim a name wins and every later claim is refused,
    /// so a standalone command shadows a Spore's command of the same name
    /// rather than colliding with it. A root that does not exist has no
    /// commands; a root that exists and cannot be read is an error, because that
    /// is a broken machine rather than a missing package.
    ///
    /// A host reads both roots: what an operator installed by hand, and what a
    /// store installed. Nothing has to be copied between them - the install tree
    /// is the package, and its manifest already says which commands it carries.
    pub fn scan_roots(roots: &[PathBuf]) -> Result<Self> {
        let mut registry = Self::over(roots.to_vec());
        for root in roots {
            let directory = match std::fs::read_dir(root) {
                Ok(directory) => directory,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(anyhow::Error::new(error)
                        .context(format!("read command root {}", root.display())));
                }
            };
            for entry in directory {
                let entry =
                    entry.with_context(|| format!("read command root {}", root.display()))?;
                // `.install`, `.staging` and friends are a store's bookkeeping,
                // not packages.
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let package = entry.path();
                // A package may be reached through a symlink; `file_type` would
                // report the link itself and skip it.
                if !package.is_dir() {
                    continue;
                }
                registry.collect(&package);
            }
        }
        Ok(registry)
    }

    fn over(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            commands: BTreeMap::new(),
            refused: Vec::new(),
        }
    }

    /// Add every command one package directory declares, or record why it
    /// cannot. A package is a unit: one broken command refuses all of them.
    fn collect(&mut self, package: &Path) {
        let entries = if package.join("command.toml").is_file() {
            standalone_command(package).map(|entry| vec![entry])
        } else if package.join("spore.toml").is_file() {
            // A Spore installs commands too. The package owns both faces - a
            // canvas and a terminal program - and the shell should not care
            // which kind of manifest declared them.
            spore_commands(package)
        } else {
            return;
        };
        match entries {
            Ok(entries) => {
                for entry in entries {
                    self.offer(entry);
                }
            }
            Err(error) => self.refused.push(Refusal {
                name: package_name(package),
                reason: format!("{error:#}"),
            }),
        }
    }

    fn offer(&mut self, entry: CommandEntry) {
        if let Some(missing) = unsupported(&entry.requires) {
            self.refused.push(Refusal {
                name: entry.name.clone(),
                reason: format!(
                    "needs {}, which this runner does not link (it links {})",
                    missing.join(", "),
                    RUNNER_CAPABILITIES.join(", ")
                ),
            });
            return;
        }
        if let Some(existing) = self.commands.get(&entry.name) {
            self.refused.push(Refusal {
                name: entry.name.clone(),
                reason: format!(
                    "{} is already claimed by {}",
                    entry.package.display(),
                    existing.package.display()
                ),
            });
            return;
        }
        self.commands.insert(entry.name.clone(), entry);
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn entries(&self) -> impl Iterator<Item = &CommandEntry> {
        self.commands.values()
    }

    pub fn path(&self) -> Vec<&str> {
        self.commands.keys().map(String::as_str).collect()
    }

    pub fn get(&self, name: &str) -> Option<&CommandEntry> {
        self.commands.get(name)
    }

    /// Names that lost a collision, and packages that could not be read.
    pub fn refused(&self) -> &[Refusal] {
        &self.refused
    }
}

/// A shim is ours if it says so. A hand-written one carries policy this tool
/// has no business replacing.
pub const SHIM_MARKER: &str = "generated by soil-command";

/// What [`write_shims`] did, so a caller can say it out loud.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ShimReport {
    pub written: Vec<PathBuf>,
    /// Names left alone, and why. A hand-written shim is not a mistake.
    pub kept: Vec<(String, String)>,
    pub pruned: Vec<PathBuf>,
}

/// Write one executable per command, so a shell that is not Mush can run it.
///
/// A shim is three lines and carries no policy: the runner and the roots are
/// baked in so it works from any directory, and argv, stdin and the exit code
/// are the caller's. Anything that does not carry [`SHIM_MARKER`] is left
/// untouched - `mb` is a hand-written shim that owns a records binding and an
/// actor, and replacing it would be a data-loss bug, not an update.
///
/// `prune` removes shims this tool wrote for commands that no longer exist. It
/// never removes a file it cannot claim.
pub fn write_shims(
    registry: &CommandRegistry,
    dir: &Path,
    runner: &Path,
    prune: bool,
) -> Result<ShimReport> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create shim directory {}", dir.display()))?;
    let mut report = ShimReport::default();
    for entry in registry.entries() {
        let target = dir.join(&entry.name);
        if target.exists() {
            match std::fs::read_to_string(&target) {
                Ok(existing) if existing.contains(SHIM_MARKER) => {}
                Ok(_) => {
                    report.kept.push((
                        entry.name.clone(),
                        "a hand-written shim is in the way".into(),
                    ));
                    continue;
                }
                Err(error) => {
                    report
                        .kept
                        .push((entry.name.clone(), format!("unreadable: {error}")));
                    continue;
                }
            }
        }
        std::fs::write(&target, shim_script(entry, registry.roots(), runner))
            .with_context(|| format!("write {}", target.display()))?;
        make_executable(&target)?;
        report.written.push(target);
    }
    if prune {
        let known: std::collections::BTreeSet<&str> = registry.path().into_iter().collect();
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("read shim directory {}", dir.display()))?
        {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if known.contains(name.as_str()) {
                continue;
            }
            let owned = std::fs::read_to_string(entry.path())
                .is_ok_and(|text| text.contains(SHIM_MARKER));
            if owned && std::fs::remove_file(entry.path()).is_ok() {
                report.pruned.push(entry.path());
            }
        }
    }
    Ok(report)
}

fn shim_script(entry: &CommandEntry, roots: &[PathBuf], runner: &Path) -> String {
    let mut script = format!(
        "#!/bin/sh\n\
         # {SHIM_MARKER} for the {} command. Do not edit; regenerate it.\n\
         # Source: {}\n\
         #\n\
         # A command gets no ambient policy. What it may reach is this file, which a\n\
         # person writes; without a records binding a command that imports\n\
         # soil:records fails to start, and says so.\n\
         policy=\"${{XDG_CONFIG_HOME:-$HOME/.config}}/fungos/commands/{}.env\"\n\
         if [ -r \"$policy\" ]; then . \"$policy\"; fi\n",
        entry.name,
        entry.package.display(),
        entry.name
    );
    let mut roots_flag = String::new();
    for root in roots {
        roots_flag.push_str(" --root ");
        roots_flag.push_str(&shell_quote(&root.to_string_lossy()));
    }
    let runner = shell_quote(&runner.to_string_lossy());
    let name = shell_quote(&entry.name);
    script.push_str(&format!(
        "book=\"${{FUNGOS_RECORDS_BOOK:-${{RECORDS_BOOK:-}}}}\"\n\
         if [ -n \"$book\" ]; then\n\
         \x20   exec {runner}{roots_flag} \\\n\
         \x20       --records-root \"${{FUNGOS_RECORDS_ROOT:-${{RECORDS_ROOT:-$HOME/.local/share/fungos/records}}}}\" \\\n\
         \x20       --book \"$book\" \\\n\
         \x20       --collection \"${{FUNGOS_RECORDS_COLLECTION:-${{RECORDS_COLLECTION:-records}}}}\" \\\n\
         \x20       run {name} \"$@\"\n\
         fi\n\
         exec {runner}{roots_flag} run {name} \"$@\"\n"
    ));
    script
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// A `soil:*` capability the runner cannot link, if the package declares one.
///
/// `wasi:*` is always satisfied: the runner is a WASI p2 host. A `soil:*` grant
/// is only satisfied if this runner links it, and the caller is told which one
/// rather than handed a linker error at the first run.
fn unsupported(requires: &[String]) -> Option<Vec<String>> {
    let missing: Vec<String> = requires
        .iter()
        .filter(|grant| {
            grant
                .strip_prefix("soil:")
                .is_some_and(|_| !RUNNER_CAPABILITIES.contains(&grant.as_str()))
        })
        .cloned()
        .collect();
    (!missing.is_empty()).then_some(missing)
}

fn package_name(package: &Path) -> String {
    package.file_name().map_or_else(
        || package.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// One package that stands alone: `command.toml` beside its module.
fn standalone_command(package: &Path) -> Result<CommandEntry> {
    let manifest_path = package.join("command.toml");
    let source = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Manifest =
        toml::from_str(&source).with_context(|| format!("parse {}", manifest_path.display()))?;
    if manifest.schema != 1 {
        bail!("unsupported command manifest schema {}", manifest.schema);
    }
    if !slug(&manifest.command.name) {
        bail!("invalid command name {:?}", manifest.command.name);
    }
    if manifest.command.summary.trim().is_empty() {
        bail!("command {} has no summary", manifest.command.name);
    }
    let module = package.join(&manifest.command.module);
    if !module.is_file() {
        bail!("command {} module missing", manifest.command.name);
    }
    Ok(CommandEntry {
        name: manifest.command.name,
        version: manifest.command.version,
        summary: manifest.command.summary,
        package: package.to_path_buf(),
        // A standalone `command.toml` has no grants section; it is the package's
        // whole contract, and the runner either satisfies it or does not.
        requires: Vec::new(),
        module,
        timeout_ms: manifest.limits.timeout_ms,
        max_stdout_bytes: manifest.limits.max_stdout_bytes,
        max_stderr_bytes: manifest.limits.max_stderr_bytes,
        max_memory_bytes: manifest.limits.max_memory_bytes,
    })
}

/// The commands an installed Spore declares.
///
/// Limits are the command defaults: a Spore declares what it runs, not how much
/// memory the host may give it.
fn spore_commands(package: &Path) -> Result<Vec<CommandEntry>> {
    if !package.join("spore.toml").is_file() {
        return Ok(Vec::new());
    }
    let manifest = soil_spore::Manifest::load(package)
        .map_err(|error| anyhow::anyhow!("{}: {error}", package.display()))?;
    let limits = Limits::default();
    let mut entries = Vec::new();
    for command in &manifest.commands {
        let module = package.join(&command.module);
        if !module.is_file() {
            bail!("spore command {} module missing", command.name);
        }
        entries.push(CommandEntry {
            name: command.name.clone(),
            version: manifest.spore.version.clone(),
            summary: command.summary.clone(),
            package: package.to_path_buf(),
            requires: manifest.grants.keys().cloned().collect(),
            module,
            timeout_ms: limits.timeout_ms,
            max_stdout_bytes: limits.max_stdout_bytes,
            max_stderr_bytes: limits.max_stderr_bytes,
            max_memory_bytes: limits.max_memory_bytes,
        });
    }
    Ok(entries)
}

#[derive(Debug, Clone)]
pub struct RecordsBinding {
    pub root: PathBuf,
    pub book: String,
    pub collection: String,
}

#[derive(Debug, Clone, Default)]
pub struct RunRequest {
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunOutput {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub struct CommandRunner {
    engine: Engine,
    records: Option<RecordsBinding>,
    components: Mutex<BTreeMap<PathBuf, Arc<wasmtime::component::Component>>>,
}

impl CommandRunner {
    pub fn new(records: Option<RecordsBinding>) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        // Keep compiled components on disk. `mb` is invoked hundreds of times a
        // day for the same 600 KB module, and without this every one of those
        // invocations pays to compile it again. Failing to open the cache is not
        // fatal: wasmtime falls back to compiling, which is the status quo ante.
        match wasmtime::Cache::new(wasmtime::CacheConfig::default()) {
            Ok(cache) => {
                config.cache(Some(cache));
            }
            Err(error) => eprintln!("soil-command: component cache unavailable: {error}"),
        }
        let engine = Engine::new(&config)?;
        Ok(Self {
            engine,
            records,
            components: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn run(&self, entry: &CommandEntry, request: &RunRequest) -> Result<RunOutput> {
        let component = {
            let mut cache = self
                .components
                .lock()
                .map_err(|_| anyhow::anyhow!("command component cache poisoned"))?;
            if let Some(component) = cache.get(&entry.module) {
                component.clone()
            } else {
                let component = Arc::new(
                    wasmtime::component::Component::from_file(&self.engine, &entry.module)
                        .map_err(|error| {
                            anyhow::anyhow!("load command {}: {error}", entry.module.display())
                        })?,
                );
                cache.insert(entry.module.clone(), component.clone());
                component
            }
        };
        let mut linker = wasmtime::component::Linker::<Host>::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        let records = match &self.records {
            Some(binding) => Some(
                RecordsBookHost::open(
                    &binding.root,
                    binding.book.clone(),
                    binding.collection.clone(),
                )
                .context("open records book for command")?,
            ),
            None => None,
        };
        if records.is_some() {
            soil_host::records_host::add_to_linker::<Host>(&mut linker)?;
        }

        let stdout = MemoryOutputPipe::new(entry.max_stdout_bytes);
        let stderr = MemoryOutputPipe::new(entry.max_stderr_bytes);
        let mut wasi = WasiCtxBuilder::new();
        let mut argv = vec![entry.name.clone()];
        argv.extend(request.args.iter().cloned());
        wasi.args(&argv);
        wasi.envs(&request.env);
        if let Some(cwd) = &request.cwd {
            wasi.env("PWD", cwd);
        }
        wasi.stdin(MemoryInputPipe::new(request.stdin.clone()));
        wasi.stdout(stdout.clone());
        wasi.stderr(stderr.clone());

        let limits = StoreLimitsBuilder::new()
            .memory_size(entry.max_memory_bytes)
            .build();
        let mut store = Store::new(
            &self.engine,
            Host {
                wasi: wasi.build(),
                table: wasmtime::component::ResourceTable::new(),
                limits,
                records,
            },
        );
        store.limiter(|host| &mut host.limits);
        store.set_epoch_deadline(1);
        let engine = self.engine.clone();
        let timeout = entry.timeout_ms;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(timeout));
            engine.increment_epoch();
        });

        let command = Command::instantiate(&mut store, &component, &linker)?;
        // A guest that traps has usually already said why on stderr — a panic
        // message lands there before the abort. Returning the trap alone throws
        // that away and leaves a wasm backtrace with no cause, so carry whatever
        // the command managed to write.
        let result = match command.wasi_cli_run().call_run(&mut store) {
            Ok(result) => result,
            Err(error) => {
                let said = String::from_utf8_lossy(&stderr.contents()).trim().to_string();
                if said.is_empty() {
                    return Err(error.into());
                }
                return Err(anyhow::anyhow!("{error}\n{said}"));
            }
        };
        Ok(RunOutput {
            code: if result.is_ok() { 0 } else { 1 },
            stdout: stdout.contents().to_vec(),
            stderr: stderr.contents().to_vec(),
        })
    }
}

struct Host {
    wasi: WasiCtx,
    table: wasmtime::component::ResourceTable,
    limits: StoreLimits,
    records: Option<RecordsBookHost>,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl soil_host::records_host::records::Host for Host {
    fn get(
        &mut self,
        key: soil_host::records_host::records::Key,
    ) -> Result<soil_host::records_host::records::Entry, soil_host::records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_host::records_host::records::Error::Denied)?
            .get(key)
    }

    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<soil_host::records_host::records::Page, soil_host::records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_host::records_host::records::Error::Denied)?
            .scan(collection, cursor, limit)
    }

    fn upsert(
        &mut self,
        entry: soil_host::records_host::records::Entry,
    ) -> Result<soil_host::records_host::records::Entry, soil_host::records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_host::records_host::records::Error::Denied)?
            .upsert(entry)
    }

    fn compare_and_swap(
        &mut self,
        key: soil_host::records_host::records::Key,
        expected: Option<soil_host::records_host::records::Version>,
        entry: soil_host::records_host::records::Entry,
    ) -> Result<soil_host::records_host::records::CasResult, soil_host::records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_host::records_host::records::Error::Denied)?
            .compare_and_swap(key, expected, entry)
    }

    fn delete(
        &mut self,
        key: soil_host::records_host::records::Key,
    ) -> Result<(), soil_host::records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_host::records_host::records::Error::Denied)?
            .delete(key)
    }

    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> Result<soil_host::records_host::records::Page, soil_host::records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_host::records_host::records::Error::Denied)?
            .changes(since, limit)
    }

    fn apply(
        &mut self,
        entries: Vec<soil_host::records_host::records::Entry>,
    ) -> Result<soil_host::records_host::records::ApplyResult, soil_host::records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_host::records_host::records::Error::Denied)?
            .apply(entries)
    }
}

/// The Spores root that sits beside a command root.
///
/// Commands and Spores are siblings under the fungOS data directory, so one
/// setting moves both. `MUSH_SPORE_ROOT` overrides the sibling when a checkout
/// keeps the two apart.
pub fn spore_root(command_root: impl AsRef<Path>) -> PathBuf {
    let command_root = command_root.as_ref();
    std::env::var_os("MUSH_SPORE_ROOT")
        .map(PathBuf::from)
        .or_else(|| command_root.parent().map(|parent| parent.join("spores")))
        .unwrap_or_else(|| PathBuf::from("spores"))
}

/// Every root a shell reads, most specific first: the command packages an
/// operator installed, then the Spores a store installed.
///
/// The order is the whole rule for a name claimed twice, and it is why a Spore
/// needs nothing copied at install time: the registry reads its tree where the
/// install left it.
pub fn roots(command_root: impl AsRef<Path>) -> Vec<PathBuf> {
    let command_root = command_root.as_ref().to_path_buf();
    let spores = spore_root(&command_root);
    vec![command_root, spores]
}

fn slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standalone(root: &Path, name: &str) {
        let package = root.join(name);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("module.wasm"), b"\0asm").unwrap();
        std::fs::write(
            package.join("command.toml"),
            format!(
                "schema = 1\n[command]\nname = \"{name}\"\nversion = \"0.1.0\"\nsummary = \"test\"\nmodule = \"module.wasm\"\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn scans_a_standalone_command_root() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            standalone(dir.path(), name);
        }
        let registry = CommandRegistry::scan(dir.path()).unwrap();
        assert_eq!(registry.path(), vec!["a", "b"]);
        assert!(registry.get("a").is_some());
        assert!(registry.refused().is_empty());
    }

    #[test]
    fn a_name_claimed_twice_is_refused_not_fatal() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        standalone(first.path(), "mb");
        standalone(second.path(), "mb");
        standalone(second.path(), "other");

        let registry =
            CommandRegistry::scan_roots(&[first.path().to_path_buf(), second.path().to_path_buf()])
                .unwrap();

        // The first root wins, so a hand-placed command shadows an installed
        // Spore's command of the same name.
        assert_eq!(registry.path(), vec!["mb", "other"]);
        assert_eq!(registry.get("mb").unwrap().package, first.path().join("mb"));
        assert_eq!(registry.refused().len(), 1);
        assert_eq!(registry.refused()[0].name, "mb");
        assert!(
            registry.refused()[0].reason.contains("already claimed"),
            "{}",
            registry.refused()[0].reason
        );
    }

    #[test]
    fn a_root_that_does_not_exist_has_no_commands() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-installed-yet");
        let registry = CommandRegistry::scan_roots(&[missing]).unwrap();
        assert!(registry.path().is_empty());
        assert!(registry.refused().is_empty());
    }

    #[test]
    fn bookkeeping_directories_are_not_packages() {
        let dir = tempfile::tempdir().unwrap();
        standalone(dir.path(), "mb");
        let staging = dir.path().join(".staging/mb-1234");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("module.wasm"), b"\0asm").unwrap();
        std::fs::write(
            staging.join("command.toml"),
            "schema = 1\n[command]\nname = \"mb\"\nversion = \"9.9.9\"\nsummary = \"half-written\"\nmodule = \"module.wasm\"\n",
        )
        .unwrap();

        let registry = CommandRegistry::scan(dir.path()).unwrap();
        assert_eq!(registry.path(), vec!["mb"]);
        assert_eq!(registry.get("mb").unwrap().version, "0.1.0");
        assert!(registry.refused().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_package_reached_through_a_symlink_is_followed() {
        let store = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        standalone(store.path(), "mb");
        std::os::unix::fs::symlink(store.path().join("mb"), root.path().join("mb")).unwrap();

        let registry = CommandRegistry::scan(root.path()).unwrap();
        assert_eq!(registry.path(), vec!["mb"]);
    }

    #[test]
    fn a_shim_names_the_runner_the_roots_and_the_command() {
        let commands = tempfile::tempdir().unwrap();
        standalone(commands.path(), "sfx");
        let prefix = tempfile::tempdir().unwrap();
        let dir = prefix.path().join("bin");
        let roots = vec![commands.path().to_path_buf()];
        let registry = CommandRegistry::scan_roots(&roots).unwrap();

        let report =
            write_shims(&registry, &dir, Path::new("/opt/soil/soil-command"), false).unwrap();

        assert_eq!(report.written, vec![dir.join("sfx")]);
        let shim = std::fs::read_to_string(dir.join("sfx")).unwrap();
        assert!(shim.starts_with("#!/bin/sh\n"), "{shim}");
        assert!(shim.contains(SHIM_MARKER));
        // The roots are baked in, so the shim does not depend on the caller's
        // environment having FUNGOS_COMMAND_ROOT set.
        assert!(
            shim.contains(&format!("--root '{}'", commands.path().display())),
            "{shim}"
        );
        assert!(shim.contains("exec '/opt/soil/soil-command' "), "{shim}");
        // Policy is sourced, never baked in: a records binding is the host's to
        // name, and the shim only knows where to look for it.
        assert!(shim.contains("fungos/commands/sfx.env"), "{shim}");
        assert!(shim.contains("--records-root"), "{shim}");
        assert!(shim.ends_with("run 'sfx' \"$@\"\n"), "{shim}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("sfx"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "a shim has to be executable");
        }
    }

    #[test]
    fn shims_leave_a_hand_written_script_alone_and_prune_only_their_own() {
        let commands = tempfile::tempdir().unwrap();
        standalone(commands.path(), "sfx");
        let prefix = tempfile::tempdir().unwrap();
        let dir = prefix.path().join("bin");
        let runner = Path::new("/opt/soil/soil-command");

        let registry = CommandRegistry::scan(commands.path()).unwrap();
        write_shims(&registry, &dir, runner, false).unwrap();

        // A person put their own `mb` there, with a records binding and an actor.
        let policy = "#!/bin/sh\necho mine\n";
        std::fs::write(dir.join("mb"), policy).unwrap();
        standalone(commands.path(), "mb");
        let registry = CommandRegistry::scan(commands.path()).unwrap();
        let report = write_shims(&registry, &dir, runner, false).unwrap();
        assert_eq!(report.kept.len(), 1, "{report:?}");
        assert_eq!(report.kept[0].0, "mb");
        assert_eq!(std::fs::read_to_string(dir.join("mb")).unwrap(), policy);

        // `sfx` goes away; pruning takes its shim and leaves the hand-written one.
        std::fs::remove_dir_all(commands.path().join("sfx")).unwrap();
        let registry = CommandRegistry::scan(commands.path()).unwrap();
        let report = write_shims(&registry, &dir, runner, true).unwrap();
        assert_eq!(report.pruned, vec![dir.join("sfx")]);
        assert!(!dir.join("sfx").exists());
        assert_eq!(std::fs::read_to_string(dir.join("mb")).unwrap(), policy);
    }

    #[test]
    fn a_path_with_a_quote_survives_the_shim() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}

#[cfg(test)]
mod spore_command_tests {
    use super::*;

    fn scratch() -> PathBuf {
        // Parallel tests share a process, and a clock alone is not enough to keep
        // two scratch roots apart.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "soil-command-scan-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    const SPORE: &str = r#"schema = 1

[spore]
id = "dev.fungos.unibus"
name = "unibus"
version = "0.2.0"
license = "MIT"
summary = "inbox"

[engine]
soil = ">=0.1"
target = "wasm32-wasip2"

[ui]
entry = "ui/inbox.lua"

[launch]
default = "canvas"
surfaces = ["canvas"]

[[commands]]
name = "unibus"
summary = "read the inbox"
module = "bin/unibus.wasm"
"#;

    #[test]
    fn a_spore_installs_its_commands_into_the_registry() {
        let root = scratch();
        let spore = root.join("unibus");
        std::fs::create_dir_all(spore.join("bin")).expect("scratch");
        std::fs::write(spore.join("spore.toml"), SPORE).expect("manifest");
        std::fs::write(spore.join("bin/unibus.wasm"), b"\0asm").expect("module");

        let registry = CommandRegistry::scan(&root).expect("scan");
        let entry = registry.get("unibus").expect("the Spore's command");
        assert_eq!(
            entry.version, "0.2.0",
            "the package's version, not the command's"
        );
        assert_eq!(entry.summary, "read the inbox");
        assert!(entry.module.ends_with("bin/unibus.wasm"));
        assert_eq!(registry.path(), vec!["unibus"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_command_whose_module_is_missing_is_refused() {
        let root = scratch();
        let spore = root.join("unibus");
        std::fs::create_dir_all(&spore).expect("scratch");
        std::fs::write(spore.join("spore.toml"), SPORE).expect("manifest");
        // No bin/unibus.wasm: a registry that listed it would promise a program
        // that cannot run. It is refused by name, and the registry still answers.
        let registry = CommandRegistry::scan(&root).expect("a broken package is not fatal");
        assert!(registry.path().is_empty());
        assert_eq!(registry.refused().len(), 1);
        assert_eq!(registry.refused()[0].name, "unibus");
        assert!(
            registry.refused()[0].reason.contains("module missing"),
            "{}",
            registry.refused()[0].reason
        );
        let _ = std::fs::remove_dir_all(root);
    }

    const SPORE_WITH_AUDIO: &str = r#"schema = 1

[spore]
id = "dev.fungos.sfx"
name = "sfx"
version = "0.1.0"
license = "MIT"
summary = "soundboard"

[engine]
soil = ">=0.1"
target = "wasm32-wasip2"

[ui]
entry = "ui/board.lua"

[launch]
default = "canvas"
surfaces = ["canvas"]

[grants]
"wasi:cli" = "std diagnostics"
"soil:audio" = "write PCM to the room"

[[commands]]
name = "sfx"
summary = "play a pad"
module = "bin/sfx.wasm"
"#;

    #[test]
    fn a_command_that_needs_an_unlinked_capability_is_refused() {
        let root = scratch();
        let spore = root.join("sfx");
        std::fs::create_dir_all(spore.join("bin")).expect("scratch");
        std::fs::write(spore.join("spore.toml"), SPORE_WITH_AUDIO).expect("manifest");
        std::fs::write(spore.join("bin/sfx.wasm"), b"\0asm").expect("module");

        // `sfx` imports soil:audio, and this runner links only soil:records. A
        // registry that listed it would put a program on PATH that traps on
        // start, so it is refused here with the reason instead.
        let registry = CommandRegistry::scan(&root).expect("a refusal is not fatal");
        assert!(registry.path().is_empty());
        assert_eq!(registry.refused().len(), 1, "{:?}", registry.refused());
        assert_eq!(registry.refused()[0].name, "sfx");
        assert!(
            registry.refused()[0].reason.contains("soil:audio"),
            "{}",
            registry.refused()[0].reason
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_installed_spore_and_a_standalone_command_share_one_path() {
        let root = scratch();
        let commands = root.join("commands");
        let spores = root.join("spores");
        let spore = spores.join("unibus");
        std::fs::create_dir_all(spore.join("bin")).expect("scratch");
        std::fs::create_dir_all(commands.join("mb")).expect("scratch");
        std::fs::write(spore.join("spore.toml"), SPORE).expect("manifest");
        std::fs::write(spore.join("bin/unibus.wasm"), b"\0asm").expect("module");
        std::fs::write(commands.join("mb/module.wasm"), b"\0asm").expect("module");
        std::fs::write(
            commands.join("mb/command.toml"),
            "schema = 1\n[command]\nname = \"mb\"\nversion = \"0.1.0\"\nsummary = \"tasks\"\nmodule = \"module.wasm\"\n",
        )
        .expect("manifest");

        let registry = CommandRegistry::scan_roots(&[commands, spores]).expect("scan");
        assert_eq!(registry.path(), vec!["mb", "unibus"]);
        let unibus = registry.get("unibus").expect("the Spore's command");
        assert!(unibus.module.ends_with("spores/unibus/bin/unibus.wasm"));
        assert!(registry.refused().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
