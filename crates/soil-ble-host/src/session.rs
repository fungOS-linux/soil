//! Minimal but real session state for roaming between hosts.
//!
//! Three things have to survive a host switch:
//!   * credentials  — so a second host never re-pairs or re-authenticates
//!   * ring identity — address + the strings the vendor API keys on
//!   * cursors      — where each bulk stream was drained to
//!
//! The lease is deliberately short-lived and advisory: the radio can only be
//! owned by one host at a time, and a crashed host must lose the ring quickly.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Creds {
    /// QRing REST token (`token` header). Shared between hosts.
    pub token: Option<String>,
    pub uid: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RingIdentity {
    /// Stable key for this ring: the BLE address is platform-specific on macOS
    /// (CoreBluetooth UUID), so prefer hardware_version + serial when known.
    pub ring_id: String,
    pub address: String,
    pub name: Option<String>,
    pub hardware_version: Option<String>,
    pub firmware_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub host_id: String,
    pub ring_id: String,
    pub claimed_at_ms: u64,
    pub expires_at_ms: u64,
}

impl Lease {
    pub fn is_live(&self, at_ms: u64) -> bool {
        at_ms < self.expires_at_ms
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    pub ring_id: String,
    pub stream: String,
    /// Inclusive lower bound already drained.
    pub since_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub creds: Creds,
    pub rings: Vec<RingIdentity>,
    pub lease: Option<Lease>,
    pub cursors: Vec<Cursor>,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
    session: Session,
}

#[allow(dead_code)]
impl SessionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let session = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse session {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Session::default(),
            Err(e) => return Err(e).with_context(|| format!("read session {}", path.display())),
        };
        Ok(Self { path, session })
    }

    pub fn get(&self) -> &Session {
        &self.session
    }

    /// Re-read the shared file before a lease decision. The lease is advisory
    /// and file-backed, so a host that opened hours ago must not decide from a
    /// stale copy. Cross-machine leases still need the signed Mycelium lease.
    fn reload(&mut self) -> Result<()> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                self.session = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse session {}", self.path.display()))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.session = Session::default();
            }
            Err(e) => {
                return Err(e).with_context(|| format!("read session {}", self.path.display()));
            }
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(&self.session)?;
        std::fs::write(&self.path, bytes)
            .with_context(|| format!("write session {}", self.path.display()))
    }

    pub fn upsert_ring(&mut self, ring: RingIdentity) -> Result<()> {
        // Reload before writing: credentials and cursors share this file with the
        // lease, and a stale copy must not erase another host's claim.
        self.reload()?;
        match self
            .session
            .rings
            .iter_mut()
            .find(|r| r.ring_id == ring.ring_id)
        {
            Some(existing) => *existing = ring,
            None => self.session.rings.push(ring),
        }
        self.save()
    }

    /// Claim the radio for `host_id`. Fails loudly if another host holds a live
    /// lease — that is the "don't fight over the link" rule, enforced.
    pub fn claim(&mut self, ring_id: &str, host_id: &str, ttl_ms: u64) -> Result<Lease> {
        self.claim_at(ring_id, host_id, ttl_ms, now_ms())
    }

    /// Deterministic form of [`claim`] for tests and simulated clocks. The
    /// lease rule is the same; only the clock is injected.
    pub fn claim_at(
        &mut self,
        ring_id: &str,
        host_id: &str,
        ttl_ms: u64,
        at: u64,
    ) -> Result<Lease> {
        self.reload()?;
        if let Some(current) = &self.session.lease
            && current.ring_id == ring_id
            && current.host_id != host_id
            && current.is_live(at)
        {
            anyhow::bail!(
                "ring {ring_id} is leased by {} for another {} ms",
                current.host_id,
                current.expires_at_ms.saturating_sub(at)
            );
        }
        let lease = Lease {
            host_id: host_id.to_string(),
            ring_id: ring_id.to_string(),
            claimed_at_ms: at,
            expires_at_ms: at.saturating_add(ttl_ms),
        };
        self.session.lease = Some(lease.clone());
        self.save()?;
        Ok(lease)
    }

    pub fn release(&mut self, host_id: &str) -> Result<()> {
        // Re-read first: a host that lost the lease must not wipe the winner's
        // claim with a stale in-memory copy.
        self.reload()?;
        if self
            .session
            .lease
            .as_ref()
            .is_some_and(|l| l.host_id == host_id)
        {
            self.session.lease = None;
            self.save()?;
        }
        Ok(())
    }

    pub fn cursor_for(&self, ring_id: &str, stream: &str) -> Option<u64> {
        self.session
            .cursors
            .iter()
            .find(|c| c.ring_id == ring_id && c.stream == stream)
            .map(|c| c.since_ms)
    }

    pub fn set_cursor(&mut self, ring_id: &str, stream: &str, since_ms: u64) -> Result<()> {
        self.reload()?;
        match self
            .session
            .cursors
            .iter_mut()
            .find(|c| c.ring_id == ring_id && c.stream == stream)
        {
            Some(c) => c.since_ms = since_ms,
            None => self.session.cursors.push(Cursor {
                ring_id: ring_id.to_string(),
                stream: stream.to_string(),
                since_ms,
            }),
        }
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_host_cannot_take_a_live_lease() {
        let path = std::env::temp_dir().join(format!(
            "soil-ble-session-test-{}-{}.json",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&path);

        let mut first = SessionStore::open(&path).expect("first store");
        let mut second = SessionStore::open(&path).expect("second store");
        first
            .claim_at("sim-pinetime", "host-a", 60_000, 1_000)
            .expect("first claim");

        let error = second
            .claim_at("sim-pinetime", "host-b", 60_000, 1_001)
            .expect_err("second claim must be refused");
        assert!(error.to_string().contains("is leased by"));

        // The contender can take the link after the first lease expires; a
        // lease is advisory, not a permanent lock.
        second
            .claim_at("sim-pinetime", "host-b", 60_000, 61_001)
            .expect("claim after expiry");

        first
            .release("host-a")
            .expect("release is a no-op for a lost lease");
        assert_eq!(
            first
                .get()
                .lease
                .as_ref()
                .map(|lease| lease.host_id.as_str()),
            Some("host-b"),
            "a stale host must not clear the new holder's lease"
        );
        first.release("host-b").expect("winner releases");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stale_host_write_does_not_erase_a_live_lease() {
        let path = std::env::temp_dir().join(format!(
            "soil-ble-session-write-test-{}-{}.json",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&path);

        let mut owner = SessionStore::open(&path).expect("owner store");
        let mut other = SessionStore::open(&path).expect("other store");
        owner
            .claim_at("sim-pinetime", "host-a", 60_000, 1_000)
            .expect("claim");

        other
            .upsert_ring(RingIdentity {
                ring_id: "sim-pinetime".to_string(),
                address: "platform-id".to_string(),
                name: Some("PineTime".to_string()),
                hardware_version: None,
                firmware_version: None,
            })
            .expect("upsert");

        assert_eq!(
            other
                .get()
                .lease
                .as_ref()
                .map(|lease| lease.host_id.as_str()),
            Some("host-a"),
            "a credential write must not erase the live lease"
        );
        let _ = std::fs::remove_file(&path);
    }
}
