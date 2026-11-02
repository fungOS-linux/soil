//! Soil BLE host for the COLMI ring protocol Spore.
//!
//! Loads one protocol guest and gives it exactly the `soil:ble` capability.
//! The radio transport (btleplug / BlueZ / CoreBluetooth) and the signed lease
//! stay in the host; framing and command semantics stay in the Spore.
//!
//! See `spores/colmi-ring/README.md` for the target matrix and lease model.

mod ble;
mod session;

use anyhow::{Context, Result, anyhow, bail};
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

wasmtime::component::bindgen!({ path: "wit", world: "host" });

use soil::ble::ble::Error as BleError;
use soil::ble::types::{Device, Event, Notification, Service};

const MAX_DEVICES: usize = 64;
const MAX_READ_BYTES: u32 = 512;
const MAX_WRITE_BYTES: usize = 512;
const MAX_EVENT_WAIT_MS: u32 = 1000;

struct State {
    ble: ble::Ble,
    session: session::SessionStore,
    host_id: String,
    /// Scan handles are one-based indexes into this list. They are Soil-issued
    /// and deliberately not the platform peripheral id.
    devices: Vec<String>,
    wasi: wasmtime_wasi::p2::WasiCtx,
    table: wasmtime_wasi::ResourceTable,
}

impl wasmtime_wasi::p2::IoView for State {
    fn table(&mut self) -> &mut wasmtime_wasi::ResourceTable {
        &mut self.table
    }
}

impl wasmtime_wasi::p2::WasiView for State {
    fn ctx(&mut self) -> &mut wasmtime_wasi::p2::WasiCtx {
        &mut self.wasi
    }
}

/// Short advisory lease: a crashed host must lose the radio quickly.
const LEASE_TTL_MS: u64 = 60_000;

fn io_error(e: anyhow::Error) -> BleError {
    eprintln!("ble: {e:#}");
    BleError::Io
}

fn claim_error(e: anyhow::Error) -> BleError {
    let text = format!("{e:#}");
    if text.contains("is leased by") {
        BleError::Leased
    } else {
        io_error(e)
    }
}

impl soil::ble::ble::Host for State {
    fn scan(&mut self, timeout_ms: u32) -> std::result::Result<Vec<Device>, BleError> {
        let timeout_ms = u64::from(timeout_ms.min(30_000));
        let found = self.ble.scan(timeout_ms).map_err(io_error)?;
        let found: Vec<_> = found.into_iter().take(MAX_DEVICES).collect();
        self.devices = found.iter().map(|d| d.id.clone()).collect();
        Ok(found
            .into_iter()
            .enumerate()
            .map(|(i, d)| Device {
                handle: i as u32 + 1,
                name: d.name,
                rssi: d.rssi,
                ring_id: None,
            })
            .collect())
    }

    fn connect(&mut self, device: u32) -> std::result::Result<(), BleError> {
        let index = usize::try_from(device)
            .ok()
            .and_then(|h| h.checked_sub(1))
            .ok_or(BleError::NotFound)?;
        let id = self.devices.get(index).cloned().ok_or(BleError::NotFound)?;

        // The radio is exclusive: refuse to fight another host for the ring.
        let lease = self
            .session
            .claim(&id, &self.host_id, LEASE_TTL_MS)
            .map_err(claim_error)?;
        eprintln!(
            "lease: {id} held by {} until {}",
            lease.host_id, lease.expires_at_ms
        );
        if let Err(e) = self.ble.connect(&id) {
            let _ = self.session.release(&self.host_id);
            return Err(io_error(e));
        }
        Ok(())
    }

    fn disconnect(&mut self) -> std::result::Result<(), BleError> {
        let result = self.ble.disconnect().map_err(io_error);
        if let Err(e) = self.session.release(&self.host_id) {
            eprintln!("lease release failed: {e:#}");
        }
        result
    }

    fn services(&mut self) -> std::result::Result<Vec<Service>, BleError> {
        let services = self.ble.services().map_err(io_error)?;
        Ok(services
            .into_iter()
            .map(|s| Service {
                uuid: s.uuid,
                characteristics: s
                    .characteristics
                    .into_iter()
                    .map(|c| soil::ble::types::Characteristic {
                        uuid: c.uuid,
                        readable: c.readable,
                        writable: c.writable,
                        notifiable: c.notifiable,
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
            Some(ble::BleEvent::Notification { char_uuid, data }) => {
                Ok(Some(Event::Notification(Notification { char_uuid, data })))
            }
            Some(ble::BleEvent::Disconnected { reason }) => Ok(Some(Event::Disconnected(reason))),
            None => Ok(None),
        }
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let guest_path = args
        .next()
        .context("usage: soil-ble-host <guest.wasm> [guest args...]")?;
    let guest_args: Vec<String> = args.collect();
    let host_id = std::env::var("SOIL_HOST_ID")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "host".to_string());
    let session_path =
        std::env::var("SOIL_BLE_SESSION").unwrap_or_else(|_| "soil-ble-session.json".to_string());
    let session = session::SessionStore::open(&session_path)
        .with_context(|| format!("open session {session_path}"))?;
    eprintln!("host id {host_id}, session {session_path}");

    let engine = Engine::default();
    let component = Component::from_file(&engine, &guest_path)
        .with_context(|| format!("load component {guest_path}"))?;

    let mut linker = Linker::<State>::new(&engine);
    // The guest is a wasip2 component (std pulls in wasi:io/cli), so WASI must
    // be present even though the ring protocol itself never touches it.
    wasmtime_wasi::p2::add_to_linker_sync::<State>(&mut linker).context("link wasi:cli")?;

    soil::ble::ble::add_to_linker::<State, wasmtime::component::HasSelf<State>>(
        &mut linker,
        |state| state,
    )
    .context("link soil:ble/ble")?;

    let state = State {
        ble: ble::Ble::new().context("init BLE backend")?,
        session,
        host_id,
        devices: Vec::new(),
        wasi: wasmtime_wasi::p2::WasiCtxBuilder::new()
            .inherit_stdio()
            .build(),
        table: wasmtime_wasi::ResourceTable::new(),
    };
    let mut store = Store::new(&engine, state);

    let bindings =
        Host_::instantiate(&mut store, &component, &linker).context("instantiate guest")?;

    match bindings.call_run(&mut store, &guest_args) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) => Err(anyhow!("guest failed: {msg}")),
        Err(e) => bail!("guest trapped: {e:#}"),
    }
}
