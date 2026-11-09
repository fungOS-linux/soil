//! Signed-command registry and wasm CLI runner.
//!
//! The registry is a virtual PATH: names resolve to installed command
//! directories, never to host binaries. The runner uses WASI stdio and links
//! only the host capabilities configured for that command.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use soil_appdata_host::{AppDataHost, AppDataStore, appdata};
use soil_audio_host::{AudioHost, OutputResource, RoomDirectory, audio};
use soil_vfs_host::{VfsHost, index, vfs};
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{Component, Instance, Linker, Val},
};
use wasmtime_wasi::{
    WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView,
    p2::{
        bindings::sync::Command,
        pipe::{MemoryInputPipe, MemoryOutputPipe},
    },
};

use soil_records_host::RecordsBookHost;

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

#[derive(Debug, Clone)]
pub struct RecordsBinding {
    pub root: PathBuf,
    pub book: String,
    pub collection: String,
}

/// Optional host capabilities linked into a command component.
///
/// A command is a normal WASI CLI component. The runner links only the
/// packages named here; an unset field is a denied import, not a fallback.
#[derive(Clone, Default)]
pub struct Capabilities {
    /// Workspace root for `soil:vfs`.
    pub vfs_root: Option<PathBuf>,
    /// Device-local appdata root for `soil:appdata`.
    pub appdata_root: Option<PathBuf>,
    /// App identity for appdata. Defaults to `dev.fungos.<command>`.
    pub app_id: Option<String>,
    /// Optional principal for the synced appdata book.
    pub user_id: Option<String>,
    /// Room table for `soil:audio`. Unset means audio is denied.
    pub rooms: Option<Arc<dyn RoomDirectory>>,
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
    capabilities: Capabilities,
}

impl CommandRunner {
    pub fn new(records: Option<RecordsBinding>) -> Result<Self> {
        Self::with_capabilities(records, Capabilities::default())
    }

    pub fn with_capabilities(
        records: Option<RecordsBinding>,
        capabilities: Capabilities,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        Ok(Self {
            engine,
            records,
            capabilities,
        })
    }

    /// Build the host services and linker a command component sees.
    ///
    /// Kept separate from `run` so a long-lived host can instantiate the same
    /// command once and then call its exported actions repeatedly. The audio
    /// output stays open for the lifetime of that instance, which is the whole
    /// difference between a soundboard and a process per button press.
    fn prepare(&self, entry: &CommandEntry, wasi: WasiCtx) -> Result<(Component, Linker<Host>, Store<Host>)> {
        let component = Component::from_file(&self.engine, &entry.module)
            .map_err(|error| anyhow::anyhow!("load command {}: {error}", entry.module.display()))?;
        let mut linker = Linker::<Host>::new(&self.engine);
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
            soil_records_host::add_to_linker::<Host>(&mut linker)?;
        }

        let vfs = match &self.capabilities.vfs_root {
            Some(root) => Some(
                VfsHost::open(root)
                    .with_context(|| format!("open command VFS {}", root.display()))?,
            ),
            None => None,
        };
        if vfs.is_some() {
            soil_vfs_host::add_to_linker::<Host>(&mut linker)?;
        }

        let appdata = match (&self.capabilities.appdata_root, &self.capabilities.app_id) {
            (Some(root), app_id) => {
                let app_id = app_id
                    .clone()
                    .unwrap_or_else(|| format!("dev.fungos.{}", entry.name));
                Some(
                    AppDataHost::new(root, &app_id, self.capabilities.user_id.as_deref())
                        .with_context(|| format!("open command appdata {}", root.display()))?,
                )
            }
            (None, _) => None,
        };
        if appdata.is_some() {
            soil_appdata_host::add_to_linker::<Host>(&mut linker)?;
        }

        let audio = self
            .capabilities
            .rooms
            .as_ref()
            .map(|rooms| AudioHost::new(Arc::clone(rooms)));
        if audio.is_some() {
            soil_audio_host::add_to_linker::<Host>(&mut linker)?;
        }

        let limits = StoreLimitsBuilder::new()
            .memory_size(entry.max_memory_bytes)
            .build();
        let mut store = Store::new(
            &self.engine,
            Host {
                wasi,
                table: wasmtime::component::ResourceTable::new(),
                limits,
                records,
                vfs,
                appdata,
                audio,
            },
        );
        store.limiter(|host| &mut host.limits);
        Ok((component, linker, store))
    }

    pub fn run(&self, entry: &CommandEntry, request: &RunRequest) -> Result<RunOutput> {
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
        let (component, linker, mut store) = self.prepare(entry, wasi.build())?;
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

    /// Instantiate a command once for repeated exported-function calls.
    ///
    /// Only components that export `act`/`snapshot` are useful here; a normal
    /// WASI command still instantiates, but has no exports to call.
    pub fn instantiate(&self, entry: &CommandEntry) -> Result<CommandInstance> {
        let (component, linker, mut store) =
            self.prepare(entry, WasiCtxBuilder::new().build())?;
        store.set_epoch_deadline(1);
        let engine = self.engine.clone();
        let timeout = entry.timeout_ms;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(timeout));
            engine.increment_epoch();
        });
        let instance = linker.instantiate(&mut store, &component)?;
        Ok(CommandInstance {
            engine: self.engine.clone(),
            store,
            instance,
            timeout_ms: entry.timeout_ms,
        })
    }
}

/// One instantiated command component, retained across calls.
pub struct CommandInstance {
    engine: Engine,
    store: Store<Host>,
    instance: Instance,
    timeout_ms: u64,
}

impl CommandInstance {
    /// Call one exported function whose result is `result<list<u8>, string>`.
    pub fn call_bytes(&mut self, export: &str, args: &[Val]) -> Result<Vec<u8>> {
        let function = self
            .instance
            .get_func(&mut self.store, export)
            .ok_or_else(|| anyhow::anyhow!("command has no {export} export"))?;
        self.store.set_epoch_deadline(1);
        let engine = self.engine.clone();
        let timeout = self.timeout_ms;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(timeout));
            engine.increment_epoch();
        });
        let mut results = [Val::Bool(false)];
        function.call(&mut self.store, args, &mut results)?;
        decode_bytes(results.into_iter().next().expect("one result slot"))
    }

    pub fn snapshot(&mut self) -> Result<Vec<u8>> {
        self.call_bytes("snapshot", &[])
    }

    pub fn act(&mut self, action: &str) -> Result<Vec<u8>> {
        self.call_bytes("act", &[Val::String(action.to_owned())])
    }
}

fn decode_bytes(value: Val) -> Result<Vec<u8>> {
    match value {
        Val::Result(Ok(Some(value))) => match *value {
            Val::List(values) => values
                .into_iter()
                .map(|value| match value {
                    Val::U8(byte) => Ok(byte),
                    other => bail!("command byte result contained {other:?}"),
                })
                .collect(),
            other => bail!("command result was not a byte list: {other:?}"),
        },
        Val::Result(Err(Some(value))) => bail!("command rejected: {value:?}"),
        Val::Result(Err(None)) => bail!("command rejected without a reason"),
        other => bail!("command result was not a result value: {other:?}"),
    }
}

struct Host {
    wasi: WasiCtx,
    table: wasmtime::component::ResourceTable,
    limits: StoreLimits,
    records: Option<RecordsBookHost>,
    vfs: Option<VfsHost>,
    appdata: Option<AppDataHost>,
    audio: Option<AudioHost>,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl soil_records_host::records::Host for Host {
    fn get(
        &mut self,
        key: soil_records_host::records::Key,
    ) -> Result<soil_records_host::records::Entry, soil_records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_records_host::records::Error::Denied)?
            .get(key)
    }

    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<soil_records_host::records::Page, soil_records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_records_host::records::Error::Denied)?
            .scan(collection, cursor, limit)
    }

    fn upsert(
        &mut self,
        entry: soil_records_host::records::Entry,
    ) -> Result<soil_records_host::records::Entry, soil_records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_records_host::records::Error::Denied)?
            .upsert(entry)
    }

    fn compare_and_swap(
        &mut self,
        key: soil_records_host::records::Key,
        expected: Option<soil_records_host::records::Version>,
        entry: soil_records_host::records::Entry,
    ) -> Result<soil_records_host::records::CasResult, soil_records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_records_host::records::Error::Denied)?
            .compare_and_swap(key, expected, entry)
    }

    fn delete(
        &mut self,
        key: soil_records_host::records::Key,
    ) -> Result<(), soil_records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_records_host::records::Error::Denied)?
            .delete(key)
    }

    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> Result<soil_records_host::records::Page, soil_records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_records_host::records::Error::Denied)?
            .changes(since, limit)
    }

    fn apply(
        &mut self,
        entries: Vec<soil_records_host::records::Entry>,
    ) -> Result<soil_records_host::records::ApplyResult, soil_records_host::records::Error> {
        self.records
            .as_mut()
            .ok_or(soil_records_host::records::Error::Denied)?
            .apply(entries)
    }
}

impl vfs::Host for Host {
    fn ls(&mut self, parent: String, offset: u64, limit: u32) -> Result<vfs::Page, vfs::Error> {
        self.vfs
            .as_mut()
            .ok_or(vfs::Error::Denied)?
            .ls(parent, offset, limit)
    }

    fn stat(&mut self, path: String) -> Result<vfs::Entry, vfs::Error> {
        self.vfs
            .as_mut()
            .ok_or(vfs::Error::Denied)?
            .stat(path)
    }

    fn read(&mut self, path: String, offset: u64, length: u32) -> Result<vfs::Chunk, vfs::Error> {
        self.vfs
            .as_mut()
            .ok_or(vfs::Error::Denied)?
            .read(path, offset, length)
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
        self.vfs
            .as_mut()
            .ok_or(index::Error::Denied)?
            .browse(parent, order, offset, limit)
    }

    fn search(&mut self, query: index::Query) -> Result<index::Page, index::Error> {
        self.vfs
            .as_mut()
            .ok_or(index::Error::Denied)?
            .search(query)
    }

    fn knn(&mut self, query: index::KnnQuery) -> Result<index::Page, index::Error> {
        self.vfs
            .as_mut()
            .ok_or(index::Error::Denied)?
            .knn(query)
    }

    fn state(&mut self) -> Result<index::IndexState, index::Error> {
        self.vfs
            .as_mut()
            .ok_or(index::Error::Denied)?
            .state()
    }

    fn refresh(&mut self) -> Result<index::IndexState, index::Error> {
        self.vfs
            .as_mut()
            .ok_or(index::Error::Denied)?
            .refresh()
    }
}

impl appdata::Host for Host {
    fn open(
        &mut self,
        scope: appdata::Scope,
    ) -> Result<wasmtime::component::Resource<AppDataStore>, appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .open(scope)
    }
}

impl appdata::HostStore for Host {
    fn get(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
        key: String,
    ) -> Result<Option<Vec<u8>>, appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .get(handle, key)
    }

    fn get_meta(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
        key: String,
    ) -> Result<Option<appdata::Meta>, appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .get_meta(handle, key)
    }

    fn set(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
        key: String,
        value: Vec<u8>,
    ) -> Result<u64, appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .set(handle, key, value)
    }

    fn set_if_generation(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
        key: String,
        value: Vec<u8>,
        generation: u64,
    ) -> Result<u64, appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .set_if_generation(handle, key, value, generation)
    }

    fn delete(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
        key: String,
    ) -> Result<(), appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .delete(handle, key)
    }

    fn list_keys(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
        prefix: String,
        limit: u32,
    ) -> Result<Vec<appdata::Meta>, appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .list_keys(handle, prefix, limit)
    }

    fn generation(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
    ) -> Result<u64, appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .generation(handle)
    }

    fn sync_state(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
    ) -> Result<appdata::SyncState, appdata::Error> {
        self.appdata
            .as_mut()
            .ok_or(appdata::Error::Denied)?
            .sync_state(handle)
    }

    fn drop(
        &mut self,
        handle: wasmtime::component::Resource<AppDataStore>,
    ) -> Result<(), wasmtime::Error> {
        self.appdata
            .as_mut()
            .ok_or_else(|| wasmtime::Error::msg("appdata is not linked"))?
            .drop(handle)
    }
}

impl audio::Host for Host {
    fn open(
        &mut self,
        format: audio::Format,
        room: Option<String>,
    ) -> Result<wasmtime::component::Resource<OutputResource>, audio::Error> {
        self.audio
            .as_mut()
            .ok_or(audio::Error::Denied)?
            .open(format, room)
    }

    fn rooms(&mut self) -> Result<Vec<audio::Room>, audio::Error> {
        self.audio
            .as_mut()
            .ok_or(audio::Error::Denied)?
            .rooms()
    }

    fn current_room(&mut self) -> Result<Option<audio::Room>, audio::Error> {
        self.audio
            .as_mut()
            .ok_or(audio::Error::Denied)?
            .current_room()
    }
}

impl audio::HostOutput for Host {
    fn format(
        &mut self,
        output: wasmtime::component::Resource<OutputResource>,
    ) -> Result<audio::Format, audio::Error> {
        self.audio
            .as_mut()
            .ok_or(audio::Error::Denied)?
            .format(output)
    }

    fn write(
        &mut self,
        output: wasmtime::component::Resource<OutputResource>,
        frames: Vec<f32>,
    ) -> Result<u32, audio::Error> {
        self.audio
            .as_mut()
            .ok_or(audio::Error::Denied)?
            .write(output, frames)
    }

    fn drain(
        &mut self,
        output: wasmtime::component::Resource<OutputResource>,
        timeout_ms: u32,
    ) -> Result<(), audio::Error> {
        self.audio
            .as_mut()
            .ok_or(audio::Error::Denied)?
            .drain(output, timeout_ms)
    }

    fn status(
        &mut self,
        output: wasmtime::component::Resource<OutputResource>,
    ) -> Result<audio::Status, audio::Error> {
        self.audio
            .as_mut()
            .ok_or(audio::Error::Denied)?
            .status(output)
    }

    fn close(
        &mut self,
        output: wasmtime::component::Resource<OutputResource>,
    ) -> Result<(), audio::Error> {
        self.audio
            .as_mut()
            .ok_or(audio::Error::Denied)?
            .close(output)
    }

    fn drop(
        &mut self,
        output: wasmtime::component::Resource<OutputResource>,
    ) -> Result<(), wasmtime::Error> {
        self.audio
            .as_mut()
            .ok_or_else(|| wasmtime::Error::msg("audio is not linked"))?
            .drop(output)
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
