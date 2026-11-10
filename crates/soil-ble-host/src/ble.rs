//! Backend selector for the BLE host.
//!
//! The `soil:ble` WIT path is identical whether a real radio or the simulator
//! is behind it. `SOIL_BLE_SIM=1` selects the deterministic backend; every
//! caller above this file uses the same methods.

use anyhow::Result;
use soil_ble_api::{Backend, DeviceInfo, ServiceInfo};

pub use soil_ble_api::BleEvent;

pub struct Ble {
    backend: Box<dyn Backend>,
}

impl Ble {
    pub fn new() -> Result<Self> {
        Ok(Self {
            backend: Box::new(crate::btleplug::BtleplugBackend::new()?),
        })
    }

    pub fn sim() -> Result<Self> {
        Ok(Self {
            backend: Box::new(soil_ble_sim::SimBackend::builtin()?),
        })
    }

    pub fn from_env() -> Result<Self> {
        if std::env::var_os("SOIL_BLE_SIM").is_some() {
            Self::sim()
        } else {
            Self::new()
        }
    }

    pub fn scan(&mut self, timeout_ms: u64) -> Result<Vec<DeviceInfo>> {
        self.backend.scan(timeout_ms)
    }

    pub fn connect(&mut self, id: &str) -> Result<()> {
        self.backend.connect(id)
    }

    pub fn disconnect(&mut self) -> Result<()> {
        self.backend.disconnect()
    }

    pub fn services(&self) -> Result<Vec<ServiceInfo>> {
        self.backend.services()
    }

    pub fn read(&self, uuid: &str) -> Result<Vec<u8>> {
        self.backend.read(uuid)
    }

    pub fn write(&self, uuid: &str, data: &[u8]) -> Result<()> {
        self.backend.write(uuid, data)
    }

    pub fn subscribe(&self, uuid: &str) -> Result<()> {
        self.backend.subscribe(uuid)
    }

    pub fn next_event(&self, timeout_ms: u64) -> Result<Option<BleEvent>> {
        self.backend.next_event(timeout_ms)
    }
}
