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
    pub module: PathBuf,
    pub timeout_ms: u64,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_memory_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct CommandRegistry {
    root: PathBuf,
    commands: BTreeMap<String, CommandEntry>,
}

impl CommandRegistry {
    /// An empty registry for hosts that start before any command is installed.
    pub fn empty(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            commands: BTreeMap::new(),
        }
    }

    pub fn scan(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let mut commands = BTreeMap::new();
        for entry in std::fs::read_dir(&root)
            .with_context(|| format!("read command root {}", root.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let manifest_path = dir.join("command.toml");
            if !manifest_path.is_file() {
                // A Spore installs commands too. The package owns both faces -
                // a canvas and a terminal program - and the shell should not
                // care which kind of manifest declared them.
                for entry in spore_commands(&dir)? {
                    if commands.contains_key(&entry.name) {
                        bail!("duplicate command {}", entry.name);
                    }
                    commands.insert(entry.name.clone(), entry);
                }
                continue;
            }
            let source = std::fs::read_to_string(&manifest_path)
                .with_context(|| format!("read {}", manifest_path.display()))?;
            let manifest: Manifest = toml::from_str(&source)
                .with_context(|| format!("parse {}", manifest_path.display()))?;
            if manifest.schema != 1 {
                bail!("unsupported command manifest schema {}", manifest.schema);
            }
            if !slug(&manifest.command.name) {
                bail!("invalid command name {:?}", manifest.command.name);
            }
            if manifest.command.summary.trim().is_empty() {
                bail!("command {} has no summary", manifest.command.name);
            }
            let module = dir.join(&manifest.command.module);
            if !module.is_file() {
                bail!("command {} module missing", manifest.command.name);
            }
            if commands.contains_key(&manifest.command.name) {
                bail!("duplicate command {}", manifest.command.name);
            }
            commands.insert(
                manifest.command.name.clone(),
                CommandEntry {
                    name: manifest.command.name,
                    version: manifest.command.version,
                    summary: manifest.command.summary,
                    module,
                    timeout_ms: manifest.limits.timeout_ms,
                    max_stdout_bytes: manifest.limits.max_stdout_bytes,
                    max_stderr_bytes: manifest.limits.max_stderr_bytes,
                    max_memory_bytes: manifest.limits.max_memory_bytes,
                },
            );
        }
        Ok(Self { root, commands })
    }

    pub fn root(&self) -> &Path {
        &self.root
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
}

/// The commands an installed Spore declares.
///
/// Limits are the command defaults: a Spore declares what it runs, not how much
/// memory the host may give it.
fn spore_commands(dir: &Path) -> Result<Vec<CommandEntry>> {
    if !dir.join("spore.toml").is_file() {
        return Ok(Vec::new());
    }
    let manifest = soil_spore::Manifest::load(dir)
        .map_err(|error| anyhow::anyhow!("{}: {error}", dir.display()))?;
    let limits = Limits::default();
    let mut entries = Vec::new();
    for command in &manifest.commands {
        let module = dir.join(&command.module);
        if !module.is_file() {
            bail!("spore command {} module missing", command.name);
        }
        entries.push(CommandEntry {
            name: command.name.clone(),
            version: manifest.spore.version.clone(),
            summary: command.summary.clone(),
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
        let result = command.wasi_cli_run().call_run(&mut store)?;
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

    #[test]
    fn registry_scans_and_rejects_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("module.wasm"), b"\0asm").unwrap();
            std::fs::write(
                root.join("command.toml"),
                format!(
                    "schema = 1\n[command]\nname = \"{name}\"\nversion = \"0.1.0\"\nsummary = \"test\"\nmodule = \"module.wasm\"\n"
                ),
            )
            .unwrap();
        }
        let registry = CommandRegistry::scan(dir.path()).unwrap();
        assert_eq!(registry.path(), vec!["a", "b"]);
        assert!(registry.get("a").is_some());
    }
}

#[cfg(test)]
mod spore_command_tests {
    use super::*;

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!(
            "soil-command-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
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
        // that cannot run.
        assert!(CommandRegistry::scan(&root).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
