//! Transport-agnostic BLE backend API.
//!
//! The wasmtime host and the simulator implement the same trait, so the
//! `soil:ble` WIT path is identical whether a radio is present or not. The
//! trait is deliberately small: scan, link lifecycle, GATT read/write,
//! notifications, and a bounded event wait. Everything above it is the host.

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Platform peripheral id. On macOS this is a CoreBluetooth UUID, not a MAC.
    pub id: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
}

/// One event drained from the notification pump. The characteristic UUID is
/// preserved so the guest can distinguish the command channel from DFU.
#[derive(Debug, Clone)]
pub enum BleEvent {
    Notification { char_uuid: String, data: Vec<u8> },
    Disconnected { reason: String },
}

#[derive(Debug, Clone)]
pub struct CharInfo {
    pub uuid: String,
    pub readable: bool,
    pub writable: bool,
    pub notifiable: bool,
}

#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub uuid: String,
    pub characteristics: Vec<CharInfo>,
}

/// A BLE transport. Implementations own their runtime or virtual clock; the
/// host calls these methods synchronously from wasm imports. `Send` is
/// required because the wasmtime store owns the backend.
pub trait Backend: Send {
    fn scan(&mut self, timeout_ms: u64) -> Result<Vec<DeviceInfo>>;
    fn connect(&mut self, id: &str) -> Result<()>;
    fn disconnect(&mut self) -> Result<()>;
    fn services(&self) -> Result<Vec<ServiceInfo>>;
    fn read(&self, uuid: &str) -> Result<Vec<u8>>;
    fn write(&self, uuid: &str, data: &[u8]) -> Result<()>;
    fn subscribe(&self, uuid: &str) -> Result<()>;
    fn next_event(&self, timeout_ms: u64) -> Result<Option<BleEvent>>;
}
