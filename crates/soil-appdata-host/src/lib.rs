//! Host adapter for `soil:appdata`.
//!
//! AppData is a private key/value book. The host derives the app identity from
//! the signed Spore manifest; this crate never accepts an app id or host path
//! from the guest. Local values live in a device-only records book. Synced
//! values live in a per-user/per-app records book whose op log can be sent to
//! another device by the native records sync provider.
//!
//! AppData is deliberately not Git and not content-addressed. A synced key is
//! one private per-user/per-app value, not a CID, and no URL or repository path
//! crosses the boundary.

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use fungos_records::{Record, Store};
use wasmtime::component::{HasSelf, Linker, Resource, ResourceTable};

wasmtime::component::bindgen!({
    path: "wit",
    world: "appdata-host",
    with: {
        "soil:appdata/appdata.store": AppDataStore,
    },
});

use soil::appdata::appdata;

const MAX_KEY_BYTES: usize = 256;
const MAX_LIST_KEYS: u32 = 512;
const APP_DATA_SCHEMA: &str = "soil.appdata/value@1";
const SYNCED_BOOK: &str = "appdata/synced";
const LOCAL_BOOK: &str = "appdata/local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_value_bytes: usize,
    pub max_total_bytes: usize,
    pub max_keys: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_value_bytes: 1 << 20,
            max_total_bytes: 16 << 20,
            max_keys: 4096,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AppDataConfig {
    /// Records peers that receive synced writes. The transport owns identity,
    /// grants, and retries; this host only configures the book. End-to-end
    /// encryption is deferred in `fungos-records`.
    pub sync_peers: Vec<String>,
    pub limits: Limits,
}

/// Host-owned sync status. A records sync runner calls these after a peer
/// exchange; the Spore cannot forge or advance this state.
#[derive(Debug, Default)]
struct SyncStatus {
    active: AtomicBool,
    last_success_ms: AtomicU64,
}

pub struct AppDataHost {
    table: ResourceTable,
    local: Arc<Mutex<Store>>,
    synced: Option<Arc<Mutex<Store>>>,
    limits: Limits,
    status: Arc<SyncStatus>,
}

/// One opened scope, backed by one records book.
pub struct AppDataStore {
    scope: appdata::Scope,
    store: Arc<Mutex<Store>>,
    book: &'static str,
    limits: Limits,
    status: Arc<SyncStatus>,
}

impl AppDataHost {
    /// Open local AppData for one signed app. Synced scope returns `not-ready`
    /// until a user identity is available.
    pub fn new(
        root: impl AsRef<Path>,
        app_id: &str,
        user_id: Option<&str>,
    ) -> anyhow::Result<Self> {
        Self::new_with_config(root, app_id, user_id, AppDataConfig::default())
    }

    pub fn new_with_config(
        root: impl AsRef<Path>,
        app_id: &str,
        user_id: Option<&str>,
        config: AppDataConfig,
    ) -> anyhow::Result<Self> {
        validate_component("app id", app_id)?;
        if let Some(user_id) = user_id {
            validate_component("user id", user_id)?;
        }
        let root = root.as_ref();
        let device = root.join("device_id");
        let app_root = root.join("apps").join(app_id);
        let local_root = app_root.join("local");
        let local = Store::open(&local_root, &device)?;
        let synced = match user_id {
            Some(user_id) => {
                let synced_root = app_root.join("users").join(user_id).join("synced");
                let store = Store::open(&synced_root, &device)?;
                if !config.sync_peers.is_empty() {
                    store.set_sync_peers(SYNCED_BOOK, &config.sync_peers)?;
                }
                Some(Arc::new(Mutex::new(store)))
            }
            None => None,
        };
        Ok(Self {
            table: ResourceTable::new(),
            local: Arc::new(Mutex::new(local)),
            synced,
            limits: config.limits,
            status: Arc::new(SyncStatus::default()),
        })
    }

    /// Called by the host-owned records sync runner before a peer exchange.
    pub fn mark_sync_started(&self) {
        self.status.active.store(true, Ordering::Release);
    }

    /// Called only after a peer exchange completed successfully.
    pub fn mark_sync_complete(&self) {
        self.status.active.store(false, Ordering::Release);
        self.status
            .last_success_ms
            .store(now_ms(), Ordering::Release);
    }

    /// Called when the sync runner cannot reach any peer.
    pub fn mark_offline(&self) {
        self.status.active.store(false, Ordering::Release);
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }
}

impl appdata::Host for AppDataHost {
    fn open(&mut self, scope: appdata::Scope) -> Result<Resource<AppDataStore>, appdata::Error> {
        let store = match scope {
            appdata::Scope::Local => AppDataStore {
                scope,
                store: Arc::clone(&self.local),
                book: LOCAL_BOOK,
                limits: self.limits,
                status: Arc::clone(&self.status),
            },
            appdata::Scope::Synced => {
                let Some(store) = &self.synced else {
                    return Err(appdata::Error::NotReady);
                };
                AppDataStore {
                    scope,
                    store: Arc::clone(store),
                    book: SYNCED_BOOK,
                    limits: self.limits,
                    status: Arc::clone(&self.status),
                }
            }
        };
        self.table
            .push(store)
            .map_err(|_| appdata::Error::OverBudget)
    }
}

impl appdata::HostStore for AppDataHost {
    fn get(
        &mut self,
        handle: Resource<AppDataStore>,
        key: String,
    ) -> Result<Option<Vec<u8>>, appdata::Error> {
        let store = self.table.get(&handle).map_err(|_| appdata::Error::Io)?;
        store.get(&key)
    }

    fn get_meta(
        &mut self,
        handle: Resource<AppDataStore>,
        key: String,
    ) -> Result<Option<appdata::Meta>, appdata::Error> {
        let store = self.table.get(&handle).map_err(|_| appdata::Error::Io)?;
        store.get_meta(&key)
    }

    fn set(
        &mut self,
        handle: Resource<AppDataStore>,
        key: String,
        value: Vec<u8>,
    ) -> Result<u64, appdata::Error> {
        let store = self.table.get(&handle).map_err(|_| appdata::Error::Io)?;
        store.set(&key, &value, None)
    }

    fn set_if_generation(
        &mut self,
        handle: Resource<AppDataStore>,
        key: String,
        value: Vec<u8>,
        generation: u64,
    ) -> Result<u64, appdata::Error> {
        let store = self.table.get(&handle).map_err(|_| appdata::Error::Io)?;
        store.set(&key, &value, Some(generation))
    }

    fn delete(
        &mut self,
        handle: Resource<AppDataStore>,
        key: String,
    ) -> Result<(), appdata::Error> {
        let store = self.table.get(&handle).map_err(|_| appdata::Error::Io)?;
        store.delete(&key)
    }

    fn list_keys(
        &mut self,
        handle: Resource<AppDataStore>,
        prefix: String,
        limit: u32,
    ) -> Result<Vec<appdata::Meta>, appdata::Error> {
        let store = self.table.get(&handle).map_err(|_| appdata::Error::Io)?;
        store.list_keys(&prefix, limit)
    }

    fn generation(&mut self, handle: Resource<AppDataStore>) -> Result<u64, appdata::Error> {
        let store = self.table.get(&handle).map_err(|_| appdata::Error::Io)?;
        store.generation()
    }

    fn sync_state(
        &mut self,
        handle: Resource<AppDataStore>,
    ) -> Result<appdata::SyncState, appdata::Error> {
        let store = self.table.get(&handle).map_err(|_| appdata::Error::Io)?;
        store.sync_state()
    }

    fn drop(&mut self, handle: Resource<AppDataStore>) -> Result<(), wasmtime::Error> {
        let _ = self.table.delete(handle);
        Ok(())
    }
}

impl AppDataStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, appdata::Error> {
        validate_key(key).map_err(|_| appdata::Error::Invalid)?;
        let store = self.store.lock().expect("appdata store poisoned");
        match store
            .get_optional(self.book, key)
            .map_err(|_| appdata::Error::Io)?
        {
            Some(record) if !record.deleted => Ok(Some(value(&record)?)),
            Some(_) | None => Ok(None),
        }
    }

    fn get_meta(&self, key: &str) -> Result<Option<appdata::Meta>, appdata::Error> {
        validate_key(key).map_err(|_| appdata::Error::Invalid)?;
        let store = self.store.lock().expect("appdata store poisoned");
        match store
            .get_optional(self.book, key)
            .map_err(|_| appdata::Error::Io)?
        {
            Some(record) if !record.deleted => Ok(Some(meta(&record)?)),
            Some(_) | None => Ok(None),
        }
    }

    fn set(
        &self,
        key: &str,
        bytes: &[u8],
        expected_generation: Option<u64>,
    ) -> Result<u64, appdata::Error> {
        validate_key(key).map_err(|_| appdata::Error::Invalid)?;
        if bytes.len() > self.limits.max_value_bytes {
            return Err(appdata::Error::OverBudget);
        }
        let mut store = self.store.lock().expect("appdata store poisoned");
        let current = match store
            .get_optional(self.book, key)
            .map_err(|_| appdata::Error::Io)?
        {
            Some(record) if !record.deleted => Some(record),
            Some(_) | None => None,
        };
        let current_generation = current.as_ref().map(record_generation).unwrap_or(0);
        if let Some(expected) = expected_generation
            && current_generation != expected
        {
            return Err(appdata::Error::Conflict);
        }
        let current_size = current
            .as_ref()
            .map(|record| value(record).map(|bytes| bytes.len()))
            .transpose()?
            .unwrap_or(0);
        if current.is_none() && self.live_records(&store)?.len() >= self.limits.max_keys {
            return Err(appdata::Error::OverBudget);
        }
        let total = self
            .live_records(&store)?
            .iter()
            .map(|record| value(record).map(|bytes| bytes.len()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .sum::<usize>();
        if total
            .saturating_sub(current_size)
            .saturating_add(bytes.len())
            > self.limits.max_total_bytes
        {
            return Err(appdata::Error::OverBudget);
        }
        let next_generation = now_ms().max(current_generation.saturating_add(1));
        let encoded =
            serde_json::json!({"value": BASE64.encode(bytes), "generation": next_generation});
        store
            .put_record(self.book, key, APP_DATA_SCHEMA, 1, encoded)
            .map_err(|_| appdata::Error::Io)?;
        let record = store.get(self.book, key).map_err(|_| appdata::Error::Io)?;
        Ok(record_generation(&record))
    }

    fn delete(&self, key: &str) -> Result<(), appdata::Error> {
        validate_key(key).map_err(|_| appdata::Error::Invalid)?;
        let mut store = self.store.lock().expect("appdata store poisoned");
        let record = match store
            .get_optional(self.book, key)
            .map_err(|_| appdata::Error::Io)?
        {
            Some(record) if !record.deleted => record,
            Some(_) | None => return Err(appdata::Error::NotFound),
        };
        store
            .delete_record(self.book, key, &record.schema_id, 1)
            .map_err(|_| appdata::Error::Io)?;
        Ok(())
    }

    fn list_keys(&self, prefix: &str, limit: u32) -> Result<Vec<appdata::Meta>, appdata::Error> {
        if limit == 0 || limit > MAX_LIST_KEYS || !valid_prefix(prefix) {
            return Err(appdata::Error::Invalid);
        }
        let store = self.store.lock().expect("appdata store poisoned");
        let mut records = self
            .live_records(&store)?
            .into_iter()
            .filter(|record| record.entity_id.starts_with(prefix))
            .collect::<Vec<_>>();
        records.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
        records.truncate(limit as usize);
        records.iter().map(meta).collect()
    }

    fn generation(&self) -> Result<u64, appdata::Error> {
        let store = self.store.lock().expect("appdata store poisoned");
        Ok(self
            .live_records(&store)?
            .iter()
            .map(record_generation)
            .max()
            .unwrap_or(0))
    }

    fn sync_state(&self) -> Result<appdata::SyncState, appdata::Error> {
        if self.scope == appdata::Scope::Local {
            return Ok(appdata::SyncState::Local);
        }
        if self.status.active.load(Ordering::Acquire) {
            return Ok(appdata::SyncState::Syncing);
        }
        let store = self.store.lock().expect("appdata store poisoned");
        let peers = store
            .sync_peers(self.book)
            .map_err(|_| appdata::Error::Io)?;
        if peers.is_empty() {
            return Ok(appdata::SyncState::Offline);
        }
        let pending = peers.iter().try_fold(0u64, |total, peer| {
            store
                .outbox_len(self.book, peer)
                .map(|count| total.saturating_add(count))
                .map_err(|_| appdata::Error::Io)
        })?;
        if pending > 0 {
            return Ok(appdata::SyncState::Syncing);
        }
        if self.status.last_success_ms.load(Ordering::Acquire) > 0 {
            Ok(appdata::SyncState::Synced)
        } else {
            Ok(appdata::SyncState::Offline)
        }
    }

    fn live_records(&self, store: &Store) -> Result<Vec<Record>, appdata::Error> {
        let records = store
            .scan_records(self.book, None, usize::MAX)
            .map_err(|_| appdata::Error::Io)?;
        Ok(records
            .into_iter()
            .filter(|record| !record.deleted)
            .collect())
    }
}

fn value(record: &Record) -> Result<Vec<u8>, appdata::Error> {
    let encoded = record
        .fields
        .get("value")
        .and_then(serde_json::Value::as_str)
        .ok_or(appdata::Error::Io)?;
    BASE64.decode(encoded).map_err(|_| appdata::Error::Io)
}

fn record_generation(record: &Record) -> u64 {
    record
        .fields
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(record.updated_hlc)
        .max(record.updated_hlc)
}

fn meta(record: &Record) -> Result<appdata::Meta, appdata::Error> {
    Ok(appdata::Meta {
        key: record.entity_id.clone(),
        generation: record_generation(record),
        size: value(record)?.len() as u64,
        updated_at_ms: record.updated_hlc,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn validate_component(label: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > MAX_KEY_BYTES
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':'))
        || value.starts_with('.')
        || value.ends_with('.')
    {
        anyhow::bail!("{label} is not a canonical identifier");
    }
    Ok(())
}

fn validate_key(key: &str) -> anyhow::Result<()> {
    if key.is_empty()
        || key.len() > MAX_KEY_BYTES
        || key.starts_with('/')
        || key.ends_with('/')
        || key.contains("//")
        || key
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | ':')))
        || key
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        anyhow::bail!("invalid appdata key");
    }
    Ok(())
}

fn valid_prefix(prefix: &str) -> bool {
    prefix.is_empty()
        || (prefix.len() <= MAX_KEY_BYTES
            && !prefix.starts_with('/')
            && !prefix.contains("//")
            && prefix
                .trim_end_matches('/')
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | ':')))
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: appdata::Host + appdata::HostStore + 'static,
{
    appdata::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soil::appdata::appdata::Host as _;
    use appdata::HostStore as _;

    fn r(handle: &Resource<AppDataStore>) -> Resource<AppDataStore> {
        Resource::new_own(handle.rep())
    }

    fn open_local(app: &str) -> (AppDataHost, Resource<AppDataStore>) {
        let dir = tempfile::tempdir().unwrap();
        // The test keeps the tempdir alive by leaking it; this is a small test
        // process and avoids a lifetime parameter on the public host.
        let root = dir.keep();
        let mut host = AppDataHost::new(&root, app, Some("user")).unwrap();
        let handle = host.open(appdata::Scope::Local).unwrap();
        (host, handle)
    }

    #[test]
    fn local_set_get_generation_and_delete() {
        let (mut host, handle) = open_local("dev.test.app");
        let generation = host
            .set(r(&handle), "settings".into(), b"{}".to_vec())
            .unwrap();
        assert_eq!(
            host.get(r(&handle), "settings".into()).unwrap(),
            Some(b"{}".to_vec())
        );
        assert_eq!(
            host.get_meta(r(&handle), "settings".into())
                .unwrap()
                .unwrap()
                .generation,
            generation
        );
        assert_eq!(host.generation(r(&handle)).unwrap(), generation);
        assert_eq!(
            host.sync_state(r(&handle)).unwrap(),
            appdata::SyncState::Local
        );
        host.delete(r(&handle), "settings".into()).unwrap();
        assert_eq!(host.get(r(&handle), "settings".into()).unwrap(), None);
        assert_eq!(
            host.delete(r(&handle), "settings".into()),
            Err(appdata::Error::NotFound)
        );
    }

    #[test]
    fn generation_compare_and_set_rejects_a_stale_writer() {
        let (mut host, handle) = open_local("dev.test.cas");
        let first = host
            .set(r(&handle), "dashboards/fleet".into(), b"one".to_vec())
            .unwrap();
        assert_eq!(
            host.set_if_generation(
                r(&handle),
                "dashboards/fleet".into(),
                b"two".to_vec(),
                first + 1
            ),
            Err(appdata::Error::Conflict)
        );
        let second = host
            .set_if_generation(
                r(&handle),
                "dashboards/fleet".into(),
                b"two".to_vec(),
                first,
            )
            .unwrap();
        assert!(second > first);
        assert_eq!(
            host.get(r(&handle), "dashboards/fleet".into()).unwrap(),
            Some(b"two".to_vec())
        );
    }

    #[test]
    fn list_keys_is_sorted_and_prefix_scoped() {
        let (mut host, handle) = open_local("dev.test.list");
        host.set(r(&handle), "b".into(), b"2".to_vec()).unwrap();
        host.set(r(&handle), "a".into(), b"1".to_vec()).unwrap();
        host.set(r(&handle), "dashboards/x".into(), b"x".to_vec())
            .unwrap();
        let keys = host
            .list_keys(r(&handle), "dashboards/".into(), 10)
            .unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "dashboards/x");
        let all = host.list_keys(r(&handle), String::new(), 10).unwrap();
        assert_eq!(
            all.iter().map(|meta| meta.key.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "dashboards/x"]
        );
    }

    #[test]
    fn synced_scope_requires_a_user_and_reports_online_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.keep();
        let mut host = AppDataHost::new(&root, "dev.test.sync", None).unwrap();
        assert!(matches!(
            host.open(appdata::Scope::Synced),
            Err(appdata::Error::NotReady)
        ));

        let mut host = AppDataHost::new_with_config(
            &root,
            "dev.test.sync",
            Some("user"),
            AppDataConfig {
                sync_peers: vec!["neo".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let handle = host.open(appdata::Scope::Synced).unwrap();
        assert_eq!(
            host.sync_state(r(&handle)).unwrap(),
            appdata::SyncState::Offline,
            "a configured peer is not a completed sync"
        );
        host.mark_sync_complete();
        assert_eq!(
            host.sync_state(r(&handle)).unwrap(),
            appdata::SyncState::Synced
        );
        host.set(r(&handle), "dashboard".into(), b"{}".to_vec())
            .unwrap();
        assert_eq!(
            host.sync_state(r(&handle)).unwrap(),
            appdata::SyncState::Syncing,
            "a new local write must return to syncing"
        );
    }

    #[test]
    fn values_survive_reopen_and_stay_per_app() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.keep();
        {
            let mut host = AppDataHost::new(&root, "dev.test.persist", Some("user")).unwrap();
            let handle = host.open(appdata::Scope::Local).unwrap();
            host.set(
                r(&handle),
                "settings".into(),
                b"{\"theme\":\"dark\"}".to_vec(),
            )
            .unwrap();
        }
        let mut host = AppDataHost::new(&root, "dev.test.persist", Some("user")).unwrap();
        let handle = host.open(appdata::Scope::Local).unwrap();
        assert_eq!(
            host.get(r(&handle), "settings".into()).unwrap(),
            Some(b"{\"theme\":\"dark\"}".to_vec())
        );

        let mut other = AppDataHost::new(&root, "dev.test.other", Some("user")).unwrap();
        let other_handle = other.open(appdata::Scope::Local).unwrap();
        assert_eq!(
            other.get(r(&other_handle), "settings".into()).unwrap(),
            None
        );
    }

    #[test]
    fn host_links_the_appdata_world() {
        let engine = wasmtime::Engine::new(&wasmtime::Config::new()).unwrap();
        let mut linker: Linker<AppDataHost> = Linker::new(&engine);
        add_to_linker(&mut linker).unwrap();
    }

    #[test]
    fn quota_and_key_rules_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.keep();
        let mut host = AppDataHost::new_with_config(
            &root,
            "dev.test.quota",
            Some("user"),
            AppDataConfig {
                limits: Limits {
                    max_value_bytes: 4,
                    max_total_bytes: 6,
                    max_keys: 2,
                },
                ..Default::default()
            },
        )
        .unwrap();
        let handle = host.open(appdata::Scope::Local).unwrap();
        assert_eq!(
            host.set(r(&handle), "bad key".into(), b"x".to_vec()),
            Err(appdata::Error::Invalid)
        );
        assert_eq!(
            host.set(r(&handle), "a".into(), b"12345".to_vec()),
            Err(appdata::Error::OverBudget)
        );
        host.set(r(&handle), "a".into(), b"1234".to_vec()).unwrap();
        host.set(r(&handle), "b".into(), b"12".to_vec()).unwrap();
        assert_eq!(
            host.set(r(&handle), "c".into(), b"1".to_vec()),
            Err(appdata::Error::OverBudget)
        );
    }
}
