//! Guest-mediated estimate capability; sockets and snapshots remain host-owned.
use anyhow::{Result, ensure};
use fungos_device_hal::estimate_transport::{
    EstimateClient, EstimateFailure, RemoteEstimate, TrustedEstimateReceipts, UnknownReason,
};
use s22_canvas_surface::{Availability, ModelEstimateView};
use std::{path::PathBuf, time::Duration};

pub const IMPORT: &str = "soil:device/estimate@0.1.0";
pub const RECORD_BYTES: usize = 24;
pub const ESTIMATE_CANVAS_GUEST: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/estimate-canvas.wasm"));

pub struct EstimateGrant {
    client: EstimateClient,
    max_age: Duration,
}
impl EstimateGrant {
    pub fn new(
        socket: PathBuf,
        trusted: Vec<TrustedEstimateReceipts>,
        max_age: Duration,
    ) -> std::result::Result<Self, EstimateFailure> {
        Ok(Self {
            client: EstimateClient::new(socket, trusted)?,
            max_age,
        })
    }
}
#[derive(Default)]
pub(crate) struct EstimateHost {
    pub enabled: bool,
    pub grant: Option<EstimateGrant>,
    serial: i64,
    reads: usize,
    updates: usize,
    pending: Option<(i64, Result<RemoteEstimate, EstimateFailure>)>,
    committed: Option<Result<RemoteEstimate, EstimateFailure>>,
}
impl EstimateHost {
    pub fn new(enabled: bool, grant: Option<EstimateGrant>) -> Self {
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
    fn view(&self, sample: &Result<RemoteEstimate, EstimateFailure>) -> ModelEstimateView {
        let snapshot = sample.as_ref().map_err(|e| *e).and_then(|remote| {
            remote.snapshot(self.grant.as_ref().map_or(Duration::ZERO, |g| g.max_age))
        });
        match snapshot {
            Ok(sample) => ModelEstimateView {
                percent: Some(sample.percent),
                availability: Availability::Fresh,
            },
            Err(error) => failure_view(error),
        }
    }
    /// Revalidate retention age on every presentation, not merely at token use.
    pub fn presentation(&self) -> ModelEstimateView {
        self.committed.as_ref().map_or_else(
            || {
                failure_view(if self.grant.is_none() {
                    EstimateFailure::Denied
                } else {
                    EstimateFailure::Unknown(UnknownReason::Unavailable)
                })
            },
            |s| self.view(s),
        )
    }
}
fn failure_view(error: EstimateFailure) -> ModelEstimateView {
    ModelEstimateView {
        percent: None,
        availability: match error {
            EstimateFailure::Denied => Availability::Denied,
            EstimateFailure::Stale => Availability::Stale,
            EstimateFailure::Unknown(_)
            | EstimateFailure::Backend
            | EstimateFailure::InvalidTime
            | EstimateFailure::Disconnected
            | EstimateFailure::Protocol
            | EstimateFailure::UntrustedReceipt
            | EstimateFailure::Overflow => Availability::Unknown,
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
                ensure!(capacity == RECORD_BYTES as i32, "estimate record capacity");
                let ptr = usize::try_from(ptr)?;
                let memory = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .ok_or_else(|| anyhow::anyhow!("estimate memory"))?;
                let end = ptr
                    .checked_add(RECORD_BYTES)
                    .ok_or_else(|| anyhow::anyhow!("estimate bounds"))?;
                ensure!(end <= memory.data_size(&caller), "estimate memory bounds");
                let host = &mut caller.data_mut().4;
                host.reads += 1;
                ensure!(host.reads <= 1, "estimate read budget");
                // Permission checked before any HAL socket access.
                let sample = host
                    .grant
                    .as_ref()
                    .ok_or(EstimateFailure::Denied)
                    .and_then(|g| g.client.read());
                host.serial = host
                    .serial
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("estimate token exhaustion"))?;
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
                let flags: u32 = u32::from(view.percent.is_some());
                record[12..16].copy_from_slice(&flags.to_le_bytes());
                record[16..20].copy_from_slice(&u32::from(view.percent.unwrap_or(0)).to_le_bytes());
                // Reserved bytes 20..24 are always zero.
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
                let host = &mut caller.data_mut().4;
                host.updates += 1;
                ensure!(host.updates <= 1, "estimate update budget");
                let (expected, sample) = host
                    .pending
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("estimate token absent or consumed"))?;
                ensure!(token == expected, "estimate token mismatch");
                // Only host-owned data is committed. Guest numeric memory is never read.
                let view = host.view(&sample);
                host.committed = Some(sample);
                if let Some(surface) = &mut caller.data_mut().2.surface {
                    surface.update_model_estimate(view)?;
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
    fn retained_model_expires_without_numeric_fallback() {
        let receipts = TrustedEstimateReceipts {
            profile_id: "synthetic-profile".into(),
            profile_sha256: "a".repeat(64),
            algorithm_id: "synthetic-algorithm".into(),
            algorithm_sha256: "b".repeat(64),
            ocv_qualification_id: "synthetic-ocv".into(),
            temperature_qualification_id: "synthetic-temp".into(),
        };
        let frame = format!(
            "FUNGOS-ESTIMATE/1\tOK\t57\t{}\t{}\t{}\t{}\t{}\t{}\t0\t0\t100\t100\n",
            receipts.profile_id,
            receipts.profile_sha256,
            receipts.algorithm_id,
            receipts.algorithm_sha256,
            receipts.ocv_qualification_id,
            receipts.temperature_qualification_id
        );
        let client = EstimateClient::new("/unused", vec![receipts.clone()]).unwrap();
        let remote = client
            .decode_response(frame.as_bytes(), std::time::Instant::now())
            .unwrap();
        let mut host = EstimateHost::new(
            true,
            Some(
                EstimateGrant::new("/unused".into(), vec![receipts], Duration::from_millis(100))
                    .unwrap(),
            ),
        );
        let retained = Ok(remote);
        assert_eq!(host.view(&retained).percent, Some(57));
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(host.view(&retained).availability, Availability::Stale);
        host.committed = Some(retained);
        assert_eq!(host.presentation().percent, None);
        assert_eq!(host.presentation().availability, Availability::Stale);
        host.reset();
        assert_eq!(host.presentation().availability, Availability::Unknown);
        assert_eq!(host.presentation().percent, None);
    }
}
