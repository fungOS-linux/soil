//! Desktop development host: BLE + Records + Unibus + monotonic time.
//!
//! This is deliberately a composition, not a new capability. The BLE backend
//! and lease are the same ones `soil-ble-host` uses; Records and Unibus are the
//! canonical host adapters. It exists so a device Spore can run end to end
//! against the simulator (or a real radio with `--real`) on a laptop.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use soil_ble_host::ble as ble_host;
use soil_ble_host::session;
use soil_host::records_host::{self, Namespace, NamespacedRecordsHost, records};
use soil_wasi_host::{MemoryBus, WasiHost, WasiHostBuilder, WasiHostPolicy, unibus::bus};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtxView, WasiView};

wasmtime::component::bindgen!({ path: "wit", world: "runner" });

use soil::ble::ble::Error as BleError;
use soil::ble::types::{Characteristic, Device, Event, Notification, Service};

const MAX_DEVICES: usize = 64;
const MAX_READ_BYTES: u32 = 512;
const MAX_WRITE_BYTES: usize = 512;
const MAX_EVENT_WAIT_MS: u32 = 1_000;
const LEASE_TTL_MS: u64 = 60_000;

struct DevHost {
    ble: ble_host::Ble,
    session: session::SessionStore,
    host_id: String,
    /// One-based handles into the last scan result.
    devices: Vec<String>,
    records: NamespacedRecordsHost,
    wasi: WasiHost,
    bus: Arc<MemoryBus>,
    start: Instant,
    /// Dev-only stable epoch. Records written by consecutive dev-host runs must
    /// be comparable; a bare `Instant` resets every process.
    time_origin_ms: u64,
}

impl WasiView for DevHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        self.wasi.ctx()
    }
}

impl records::Host for DevHost {
    fn get(&mut self, key: records::Key) -> std::result::Result<records::Entry, records::Error> {
        records::Host::get(&mut self.records, key)
    }

    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> std::result::Result<records::Page, records::Error> {
        records::Host::scan(&mut self.records, collection, cursor, limit)
    }

    fn upsert(
        &mut self,
        entry: records::Entry,
    ) -> std::result::Result<records::Entry, records::Error> {
        records::Host::upsert(&mut self.records, entry)
    }

    fn compare_and_swap(
        &mut self,
        key: records::Key,
        expected: Option<records::Version>,
        entry: records::Entry,
    ) -> std::result::Result<records::CasResult, records::Error> {
        records::Host::compare_and_swap(&mut self.records, key, expected, entry)
    }

    fn delete(&mut self, key: records::Key) -> std::result::Result<(), records::Error> {
        records::Host::delete(&mut self.records, key)
    }

    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> std::result::Result<records::Page, records::Error> {
        records::Host::changes(&mut self.records, since, limit)
    }

    fn apply(
        &mut self,
        entries: Vec<records::Entry>,
    ) -> std::result::Result<records::ApplyResult, records::Error> {
        records::Host::apply(&mut self.records, entries)
    }
}

impl bus::Host for DevHost {
    fn poll(
        &mut self,
        kinds: Vec<String>,
        cursor: Option<String>,
        limit: u32,
    ) -> std::result::Result<bus::Page, bus::Error> {
        <WasiHost as bus::Host>::poll(&mut self.wasi, kinds, cursor, limit)
    }

    fn publish(
        &mut self,
        kind: String,
        body: Vec<u8>,
        destination: Option<String>,
        ttl_ms: u64,
    ) -> std::result::Result<bus::Receipt, bus::Error> {
        <WasiHost as bus::Host>::publish(&mut self.wasi, kind, body, destination, ttl_ms)
    }
}

impl soil::time::time::Host for DevHost {
    fn monotonic_ms(&mut self) -> std::result::Result<u64, soil::time::time::Error> {
        Ok(self.now_ms())
    }

    fn tick(
        &mut self,
        timeout_ms: u32,
    ) -> std::result::Result<Option<u64>, soil::time::time::Error> {
        let wait = timeout_ms.min(1_000);
        if wait > 0 {
            std::thread::sleep(Duration::from_millis(u64::from(wait)));
        }
        Ok(Some(self.now_ms()))
    }
}

impl DevHost {
    fn now_ms(&self) -> u64 {
        self.time_origin_ms
            .saturating_add(u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX))
    }
}

impl soil::ble::ble::Host for DevHost {
    fn scan(&mut self, timeout_ms: u32) -> std::result::Result<Vec<Device>, BleError> {
        let timeout_ms = u64::from(timeout_ms.min(30_000));
        let found = self.ble.scan(timeout_ms).map_err(io_error)?;
        let found: Vec<_> = found.into_iter().take(MAX_DEVICES).collect();
        self.devices = found.iter().map(|device| device.id.clone()).collect();
        Ok(found
            .into_iter()
            .enumerate()
            .map(|(index, device)| Device {
                handle: index as u32 + 1,
                name: device.name,
                rssi: device.rssi,
                device_id: None,
            })
            .collect())
    }

    fn connect(&mut self, device: u32) -> std::result::Result<(), BleError> {
        let index = usize::try_from(device)
            .ok()
            .and_then(|handle| handle.checked_sub(1))
            .ok_or(BleError::NotFound)?;
        let id = self.devices.get(index).cloned().ok_or(BleError::NotFound)?;

        let lease = self
            .session
            .claim(&id, &self.host_id, LEASE_TTL_MS)
            .map_err(claim_error)?;
        eprintln!(
            "lease: {id} held by {} until {}",
            lease.host_id, lease.expires_at_ms
        );
        if let Err(error) = self.ble.connect(&id) {
            let _ = self.session.release(&self.host_id);
            return Err(io_error(error));
        }
        Ok(())
    }

    fn disconnect(&mut self) -> std::result::Result<(), BleError> {
        let result = self.ble.disconnect().map_err(io_error);
        if let Err(error) = self.session.release(&self.host_id) {
            eprintln!("lease release failed: {error:#}");
        }
        result
    }

    fn services(&mut self) -> std::result::Result<Vec<Service>, BleError> {
        let services = self.ble.services().map_err(io_error)?;
        Ok(services
            .into_iter()
            .map(|service| Service {
                uuid: service.uuid,
                characteristics: service
                    .characteristics
                    .into_iter()
                    .map(|characteristic| Characteristic {
                        uuid: characteristic.uuid,
                        readable: characteristic.readable,
                        writable: characteristic.writable,
                        notifiable: characteristic.notifiable,
                    })
                    .collect(),
            })
            .collect())
    }

    fn read(
        &mut self,
        char_uuid: String,
        max_bytes: u32,
    ) -> std::result::Result<Vec<u8>, BleError> {
        if char_uuid.is_empty() {
            return Err(BleError::Invalid);
        }
        if max_bytes == 0 || max_bytes > MAX_READ_BYTES {
            return Err(BleError::OverBudget);
        }
        let data = self.ble.read(&char_uuid).map_err(io_error)?;
        Ok(data.into_iter().take(max_bytes as usize).collect())
    }

    fn write(&mut self, char_uuid: String, data: Vec<u8>) -> std::result::Result<(), BleError> {
        if char_uuid.is_empty() {
            return Err(BleError::Invalid);
        }
        if data.len() > MAX_WRITE_BYTES {
            return Err(BleError::OverBudget);
        }
        self.ble.write(&char_uuid, &data).map_err(io_error)
    }

    fn subscribe(&mut self, char_uuid: String) -> std::result::Result<(), BleError> {
        if char_uuid.is_empty() {
            return Err(BleError::Invalid);
        }
        self.ble.subscribe(&char_uuid).map_err(io_error)
    }

    fn next_event(&mut self, timeout_ms: u32) -> std::result::Result<Option<Event>, BleError> {
        let timeout_ms = u64::from(timeout_ms.min(MAX_EVENT_WAIT_MS));
        match self.ble.next_event(timeout_ms).map_err(io_error)? {
            Some(ble_host::BleEvent::Notification { char_uuid, data }) => {
                Ok(Some(Event::Notification(Notification { char_uuid, data })))
            }
            Some(ble_host::BleEvent::Disconnected { reason }) => {
                Ok(Some(Event::Disconnected(reason)))
            }
            None => Ok(None),
        }
    }
}

/// Persist the dev host's epoch in the records directory. Each run starts at
/// the later of the stored value and the current wall time, so records from
/// consecutive runs share one forward-moving timeline. This is a development
/// concession, not the production `soil:time` contract.
fn load_or_create_origin(path: &Path) -> Result<u64> {
    let persisted = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    let value = persisted.max(wall);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, value.to_string()).with_context(|| format!("write {}", path.display()))?;
    Ok(value)
}

fn io_error(error: anyhow::Error) -> BleError {
    eprintln!("ble: {error:#}");
    BleError::Io
}

fn claim_error(error: anyhow::Error) -> BleError {
    let text = format!("{error:#}");
    if text.contains("is leased by") {
        BleError::Leased
    } else {
        io_error(error)
    }
}

fn main() -> Result<()> {
    let mut real = false;
    let mut records_dir =
        std::env::var("SOIL_RECORDS_ROOT").unwrap_or_else(|_| ".soil-dev/records".to_string());
    let mut session_path = std::env::var("SOIL_BLE_SESSION")
        .unwrap_or_else(|_| ".soil-dev/soil-ble-session.json".to_string());
    let mut host_id = std::env::var("SOIL_HOST_ID")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "dev-host".to_string());

    let mut args = std::env::args().skip(1);
    let mut guest_path = None;
    let mut guest_args = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--real" => real = true,
            "--records-dir" => records_dir = args.next().context("--records-dir needs a path")?,
            "--session" => session_path = args.next().context("--session needs a path")?,
            "--host-id" => host_id = args.next().context("--host-id needs a value")?,
            "--help" | "-h" => {
                println!(
                    "usage: soil-dev-host [--real] [--records-dir DIR] [--session FILE] [--host-id ID] <guest.wasm> [guest args...]"
                );
                return Ok(());
            }
            _ if guest_path.is_none() => guest_path = Some(arg),
            _ => {
                guest_args.push(arg);
                guest_args.extend(args);
                break;
            }
        }
    }
    let guest_path = guest_path.context(
        "usage: soil-dev-host [--real] [--records-dir DIR] [--session FILE] [--host-id ID] <guest.wasm> [guest args...]",
    )?;

    let ble = if real {
        ble_host::Ble::new().context("init real BLE backend")?
    } else {
        ble_host::Ble::sim_paced(Duration::from_millis(50)).context("init simulated BLE backend")?
    };
    let session = session::SessionStore::open(&session_path)
        .with_context(|| format!("open session {session_path}"))?;
    let records = NamespacedRecordsHost::open(
        &records_dir,
        vec![
            Namespace::write("signal", "dev/personal/telemetry", "signal"),
            Namespace::write("device.state", "dev/personal/telemetry", "device.state"),
            Namespace::write("vitals.summary", "dev/personal/telemetry", "vitals.summary"),
            Namespace::write("vitals.latest", "dev/personal/telemetry", "vitals.latest"),
            Namespace::write("vitals.view", "dev/personal/telemetry", "vitals.view"),
        ],
    )
    .with_context(|| format!("open records {records_dir}"))?;
    let time_origin_ms = load_or_create_origin(&Path::new(&records_dir).join("time-origin-ms"))?;
    let bus = Arc::new(MemoryBus::default());
    let wasi = WasiHostBuilder::new(WasiHostPolicy::default())
        .inherit_stdio(true)
        .bus(bus.clone())
        .build()
        .context("build WASI state")?;

    let state = DevHost {
        ble,
        session,
        host_id,
        devices: Vec::new(),
        records,
        wasi,
        bus,
        start: Instant::now(),
        time_origin_ms,
    };

    let engine = Engine::default();
    let component = Component::from_file(&engine, &guest_path)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("load component {guest_path}"))?;
    let mut linker = Linker::<DevHost>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(anyhow::Error::from)
        .context("link wasi:cli")?;
    soil::ble::ble::add_to_linker::<DevHost, HasSelf<DevHost>>(&mut linker, |state| state)
        .map_err(anyhow::Error::from)
        .context("link soil:ble")?;
    soil::time::time::add_to_linker::<DevHost, HasSelf<DevHost>>(&mut linker, |state| state)
        .map_err(anyhow::Error::from)
        .context("link soil:time")?;
    records_host::add_to_linker::<DevHost>(&mut linker).context("link soil:records")?;
    soil_wasi_host::unibus::add_to_linker::<DevHost>(&mut linker).context("link soil:unibus")?;

    let mut store = Store::new(&engine, state);
    let bindings = Runner::instantiate(&mut store, &component, &linker)
        .map_err(anyhow::Error::from)
        .context("instantiate guest")?;
    match bindings.call_run(&mut store, &guest_args) {
        Ok(Ok(())) => {}
        Ok(Err(message)) => bail!("guest failed: {message}"),
        Err(error) => bail!("guest trapped: {error:#}"),
    }
    match bindings.call_snapshot(&mut store) {
        Ok(Ok(bytes)) => println!("{}", String::from_utf8_lossy(&bytes)),
        Ok(Err(message)) => eprintln!("snapshot failed: {message}"),
        Err(error) => eprintln!("snapshot trapped: {error:#}"),
    }
    for frame in store.data().bus.frames() {
        println!(
            "bus {} {}",
            frame.kind,
            String::from_utf8_lossy(&frame.body)
        );
    }
    Ok(())
}
