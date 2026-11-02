//! Guest-mediated battery capability; sockets and snapshots remain host-owned.
use anyhow::{Result, ensure};
use fungos_device_hal::battery_transport::{BatteryClient, BatteryFailure, RemoteBatterySample};
use s22_canvas_surface::{Availability, BatteryView};
use std::{path::PathBuf, time::Duration};

pub const IMPORT: &str = "soil:device/battery@0.1.0";
pub const RECORD_BYTES: usize = 24;
pub const BATTERY_CANVAS_GUEST: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/battery-canvas.wasm"));

pub struct BatteryGrant {
    client: BatteryClient,
    max_age: Duration,
    /// The battery changes on a human timescale; re-reading the provider on
    /// every animation frame costs a full socket round trip. Serve a recent
    /// host-owned sample instead. Retention age is still revalidated by view().
    refresh: Duration,
    cached: Option<(
        std::time::Instant,
        Result<RemoteBatterySample, BatteryFailure>,
    )>,
}
impl BatteryGrant {
    pub fn new(socket: PathBuf, max_age: Duration) -> Self {
        Self {
            client: BatteryClient::new(socket),
            max_age,
            refresh: Duration::from_millis(500),
            cached: None,
        }
    }
    /// Override the short host-side sample cache. A zero refresh forces one
    /// provider round trip per guest read, which keeps retained-token tests
    /// deterministic.
    pub fn with_refresh(mut self, refresh: Duration) -> Self {
        self.refresh = refresh;
        self
    }
    fn read(&mut self) -> Result<RemoteBatterySample, BatteryFailure> {
        if let Some((at, sample)) = &self.cached
            && at.elapsed() < self.refresh
        {
            return sample.clone();
        }
        let sample = self.client.read();
        self.cached = Some((std::time::Instant::now(), sample.clone()));
        sample
    }
}
#[derive(Default)]
pub(crate) struct BatteryHost {
    pub enabled: bool,
    pub grant: Option<BatteryGrant>,
    serial: i64,
    reads: usize,
    updates: usize,
    pending: Option<(i64, Result<RemoteBatterySample, BatteryFailure>)>,
    committed: Option<Result<RemoteBatterySample, BatteryFailure>>,
}
impl BatteryHost {
    pub fn new(enabled: bool, grant: Option<BatteryGrant>) -> Self {
        Self {
            enabled,
            grant,
            ..Default::default()
        }
    }
    pub fn reset(&mut self) {
        self.reads = 0;
        self.updates = 0;
        self.pending = None;
        self.committed = None;
    }
    fn view(&self, sample: &Result<RemoteBatterySample, BatteryFailure>) -> BatteryView {
        let snapshot = sample.as_ref().map_err(|e| *e).and_then(|remote| {
            remote.snapshot(self.grant.as_ref().map_or(Duration::ZERO, |g| g.max_age))
        });
        match snapshot {
            Ok(sample) => BatteryView {
                percent: sample.percent,
                voltage_uv: sample.voltage_microvolts,
                availability: Availability::Fresh,
            },
            Err(error) => failure_view(error),
        }
    }
    /// Revalidate retention age on every presentation, not merely at token use.
    pub fn presentation(&self) -> BatteryView {
        self.committed.as_ref().map_or_else(
            || {
                failure_view(if self.grant.is_none() {
                    BatteryFailure::Denied
                } else {
                    BatteryFailure::Unavailable
                })
            },
            |s| self.view(s),
        )
    }
}
fn failure_view(error: BatteryFailure) -> BatteryView {
    BatteryView {
        percent: None,
        voltage_uv: None,
        availability: match error {
            BatteryFailure::Denied => Availability::Denied,
            BatteryFailure::Stale => Availability::Stale,
            BatteryFailure::Unavailable
            | BatteryFailure::Backend
            | BatteryFailure::Ambiguous
            | BatteryFailure::InvalidTime
            | BatteryFailure::Disconnected
            | BatteryFailure::Protocol => Availability::Unknown,
        },
    }
}
pub(crate) fn link(linker: &mut wasmtime::Linker<crate::Limits>) -> Result<()> {
    linker.func_wrap(
        IMPORT,
        "read",
        |mut caller: wasmtime::Caller<'_, crate::Limits>,
         ptr: i32,
         capacity: i32|
         -> wasmtime::Result<i32> {
            crate::host((|| -> Result<i32> {
                ensure!(capacity == RECORD_BYTES as i32, "battery record capacity");
                let ptr = usize::try_from(ptr)?;
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| anyhow::anyhow!("battery memory"))?;
                let end = ptr
                    .checked_add(RECORD_BYTES)
                    .ok_or_else(|| anyhow::anyhow!("battery bounds"))?;
                ensure!(end <= memory.data_size(&caller), "battery memory bounds");
                let host = &mut caller.data_mut().3;
                host.reads += 1;
                ensure!(host.reads <= 1, "battery read budget");
                // Permission checked before any HAL socket access.
                let sample = host
                    .grant
                    .as_mut()
                    .ok_or(BatteryFailure::Denied)
                    .and_then(BatteryGrant::read);
                host.serial = host
                    .serial
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("battery token exhaustion"))?;
                let token = host.serial;
                let view = host.view(&sample);
                host.pending = Some((token, sample));
                let mut record = [0u8; RECORD_BYTES];
                record[..8].copy_from_slice(&token.to_le_bytes());
                let status: u32 = match view.availability {
                    Availability::Fresh => 0,
                    Availability::Unknown => 1,
                    Availability::Stale => 2,
                    Availability::Denied => 3,
                };
                record[8..12].copy_from_slice(&status.to_le_bytes());
                let flags: u32 =
                    u32::from(view.percent.is_some()) | (u32::from(view.voltage_uv.is_some()) << 1);
                record[12..16].copy_from_slice(&flags.to_le_bytes());
                record[16..20].copy_from_slice(&u32::from(view.percent.unwrap_or(0)).to_le_bytes());
                record[20..24].copy_from_slice(&view.voltage_uv.unwrap_or(0).to_le_bytes());
                memory.write(&mut caller, ptr, &record)?;
                Ok(0)
            })())
        },
    )?;
    linker.func_wrap(
        IMPORT,
        "update-status",
        |mut caller: wasmtime::Caller<'_, crate::Limits>, token: i64| -> wasmtime::Result<i32> {
            crate::host((|| -> Result<i32> {
                let host = &mut caller.data_mut().3;
                host.updates += 1;
                ensure!(host.updates <= 1, "battery update budget");
                let (expected, sample) = host
                    .pending
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("battery token absent or consumed"))?;
                ensure!(token == expected, "battery token mismatch");
                // Only host-owned data is committed. Guest numeric memory is never read.
                let view = host.view(&sample);
                host.committed = Some(sample);
                if let Some(surface) = &mut caller.data_mut().2.surface {
                    surface.update_status(view)?;
                }
                Ok(0)
            })())
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_snapshot_expires_before_consumption_or_presentation() {
        use fungos_device_hal::{
            battery_transport::{decode_response, encode_response},
            services::{BatteryProvenance, BatterySample, ChargingState},
        };
        let sample = BatterySample {
            percent: Some(42),
            voltage_microvolts: Some(3885732),
            charging: ChargingState::Unknown,
            current_microamps: None,
            temperature_deci_celsius: None,
            provenance: BatteryProvenance::LinuxPowerSupply("/sys/test".into()),
            observed_monotonic_ms: 100,
        };
        let remote = decode_response(
            encode_response(Ok(sample), 100).as_bytes(),
            std::time::Instant::now(),
        )
        .unwrap();
        let mut host = BatteryHost::new(
            true,
            Some(BatteryGrant::new(
                "/unused".into(),
                Duration::from_millis(30),
            )),
        );
        let retained = Ok(remote);
        assert_eq!(host.view(&retained).availability, Availability::Fresh);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(host.view(&retained).availability, Availability::Stale);
        host.committed = Some(retained);
        assert_eq!(host.presentation().percent, None);
        assert_eq!(host.presentation().voltage_uv, None);
    }
}
