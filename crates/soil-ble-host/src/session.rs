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

    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(&self.session)?;
        std::fs::write(&self.path, bytes)
            .with_context(|| format!("write session {}", self.path.display()))
    }

    pub fn upsert_ring(&mut self, ring: RingIdentity) -> Result<()> {
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
        let at = now_ms();
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
            expires_at_ms: at + ttl_ms,
        };
        self.session.lease = Some(lease.clone());
        self.save()?;
        Ok(lease)
    }

    pub fn release(&mut self, host_id: &str) -> Result<()> {
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
