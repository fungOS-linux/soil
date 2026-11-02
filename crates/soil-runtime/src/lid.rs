//! Permissioned read-only lid capability. Transport and hardware semantics are HAL-owned.
use fungos_device_hal::{
    lid::{LidFailure, LidPosition},
    lid_transport::LidClient,
};
use std::{path::PathBuf, time::Duration};

pub const IMPORT: &str = "soil:device/lid@0.1.0";
pub const RECORD_BYTES: usize = 288;
pub const LID_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/phone-lid.wasm"));

/// Explicit per-instance grant. Socket and freshness are deployment authority,
/// never accepted from guest memory. No grant is the default.
pub struct LidGrant {
    pub socket: PathBuf,
    pub max_age: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub failure: Option<LidFailure>,
    pub position: Option<LidPosition>,
    pub raw: Option<bool>,
    pub provider_monotonic_ms: u64,
    pub age_ms: u64,
    pub event_node: PathBuf,
}
impl Observation {
    fn failed(failure: LidFailure) -> Self {
        Self {
            failure: Some(failure),
            position: None,
            raw: None,
            provider_monotonic_ms: 0,
            age_ms: 0,
            event_node: PathBuf::new(),
        }
    }
    /// Explicit stable guest ABI codes; never Rust discriminants or Debug parsing.
    pub fn status(&self) -> u32 {
        match self.failure {
            None => 0,
            Some(LidFailure::Denied) => 1,
            Some(LidFailure::Unavailable) => 2,
            Some(LidFailure::Backend) => 3,
            Some(LidFailure::Ambiguous) => 4,
            Some(LidFailure::Resynchronizing) => 5,
            Some(LidFailure::Stale) => 6,
            Some(LidFailure::InvalidTime) => 7,
            Some(LidFailure::Disconnected) => 8,
            Some(LidFailure::Protocol) => 9,
        }
    }
    fn bytes(&self) -> [u8; RECORD_BYTES] {
        use std::os::unix::ffi::OsStrExt;
        let mut b = [0; RECORD_BYTES];
        b[0..4].copy_from_slice(&self.status().to_le_bytes());
        let position: u32 = match self.position {
            None => 0,
            Some(LidPosition::Unqualified) => 1,
            Some(LidPosition::Open) => 2,
            Some(LidPosition::Closed) => 3,
        };
        b[4..8].copy_from_slice(&position.to_le_bytes());
        b[8..12].copy_from_slice(
            &self
                .raw
                .map_or(0u32, |v| if v { 2 } else { 1 })
                .to_le_bytes(),
        );
        b[16..24].copy_from_slice(&self.provider_monotonic_ms.to_le_bytes());
        b[24..32].copy_from_slice(&self.age_ms.to_le_bytes());
        let node = self.event_node.as_os_str().as_bytes();
        b[12..16].copy_from_slice(&(node.len() as u32).to_le_bytes());
        b[32..32 + node.len()].copy_from_slice(node);
        b
    }
}

#[derive(Default)]
pub(crate) struct LidHost {
    grant: Option<LidGrant>,
    last: Option<Observation>,
    pub logs: Vec<Observation>,
    reads: usize,
}
impl LidHost {
    pub fn new(grant: Option<LidGrant>) -> Self {
        Self {
            grant,
            ..Self::default()
        }
    }
    fn read(&mut self) -> Observation {
        use std::os::unix::ffi::OsStrExt;
        self.reads += 1;
        let Some(grant) = &self.grant else {
            return Observation::failed(LidFailure::Denied);
        };
        // Bound host calls independently of guest fuel, including start functions.
        if self.reads > 1 {
            return Observation::failed(LidFailure::Backend);
        }
        let remote = match LidClient::new(grant.socket.clone()).read() {
            Ok(remote) => remote,
            Err(e) => return Observation::failed(e),
        };
        let sample = match remote.snapshot(grant.max_age) {
            Ok(sample) => sample,
            Err(e) => return Observation::failed(e),
        };
        if sample.event_node.as_os_str().as_bytes().len() > 256 {
            return Observation::failed(LidFailure::Backend);
        }
        Observation {
            failure: None,
            position: Some(sample.physical_position),
            raw: Some(sample.switch_asserted),
            provider_monotonic_ms: sample.observed_monotonic_ms,
            age_ms: remote.age().as_millis().min(u64::MAX as u128) as u64,
            event_node: sample.event_node,
        }
    }
    pub fn reset(&mut self) {
        self.reads = 0;
        self.logs.clear();
        self.last = None;
    }
}

pub(crate) fn link(linker: &mut wasmtime::Linker<crate::Limits>) -> anyhow::Result<()> {
    linker.func_wrap(
        IMPORT,
        "read",
        |mut caller: wasmtime::Caller<'_, crate::Limits>,
         ptr: i32,
         capacity: i32|
         -> wasmtime::Result<i32> {
            crate::host((|| -> anyhow::Result<i32> {
                anyhow::ensure!(
                    capacity == RECORD_BYTES as i32 && ptr >= 0,
                    "invalid lid buffer"
                );
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| anyhow::anyhow!("lid memory missing"))?;
                let end = (ptr as usize)
                    .checked_add(RECORD_BYTES)
                    .ok_or_else(|| anyhow::anyhow!("lid bounds"))?;
                anyhow::ensure!(end <= memory.data_size(&caller), "lid buffer out of bounds");
                let observation = caller.data_mut().1.read();
                let status = observation.status();
                memory.write(&mut caller, ptr as usize, &observation.bytes())?;
                caller.data_mut().1.last = Some(observation);
                Ok(status as i32)
            })())
        },
    )?;
    linker.func_wrap(
        IMPORT,
        "log-change",
        |mut caller: wasmtime::Caller<'_, crate::Limits>| -> wasmtime::Result<()> {
            crate::host((|| -> anyhow::Result<()> {
                let host = &mut caller.data_mut().1;
                anyhow::ensure!(host.logs.is_empty(), "lid log budget exceeded");
                let observation = host
                    .last
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("lid log without read"))?;
                host.logs.push(observation);
                Ok(())
            })())
        },
    )?;
    Ok(())
}
