//! The Spore Store's decision layer.
//!
//! Everything between "a signed index arrived" and "here is what the storefront
//! should show, and what applying an update would cost":
//!
//! * [`index`] parses the append-only signed catalog and resolves it to the
//!   newest entry per Spore.
//! * [`plan`] compares the catalog against what is installed and classifies each
//!   update by what applying it would require (`live`, `restart`, `consent`,
//!   `system`, `hold`).
//! * [`snapshot`] emits the table `spores/store/ui/store.lua` renders, so the
//!   view stays a pure function of a snapshot.
//!
//! The crate does not fetch, verify or install. Those are host capabilities with
//! consent in front of them; this is the part that can be reasoned about
//! offline, and it is the part every client (storefront, `store --updates`,
//! fleet policy) must agree on.

use std::fmt;

pub mod cid;
pub mod index;
pub mod listing;
pub mod media;
pub mod plan;
pub mod publish;
pub mod review;
pub mod serve;
pub mod snapshot;

pub use index::{Checkpoint, Delegation, Entry, EntryKind, Index, Revocation, SCHEMA, Verified};
pub use listing::{Listing, Shot};
pub use plan::{Class, Installed, Plan, Update, plan};
pub use review::Reviews;
pub use snapshot::{Snapshot, Source, build as build_snapshot, to_lua};

#[derive(Debug)]
pub enum Error {
    Io(String),
    Json(String),
    Invalid(String),
    Package(String),
    /// An external tool (gpg) was missing or failed.
    Tool(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "io: {message}"),
            Self::Json(message) => write!(f, "json: {message}"),
            Self::Invalid(message) => write!(f, "invalid: {message}"),
            Self::Package(message) => write!(f, "package: {message}"),
            Self::Tool(message) => write!(f, "tool: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<soil_spore::Error> for Error {
    fn from(error: soil_spore::Error) -> Self {
        Self::Package(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// One lowercase hex SHA-256, or nothing.
pub fn digest(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Lowercase hex, the only spelling the index carries.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A content-addressed reference the store will not resolve itself.
pub fn reference(value: &str) -> bool {
    let Some(cid) = value.strip_prefix("ipfs://") else {
        return false;
    };
    (20..=128).contains(&cid.len())
        && cid.starts_with('b')
        && cid.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7'))
}

/// Semver-ish ordering on the numeric core.
///
/// Pre-release and build metadata are ignored, and that is deliberate: a store
/// deciding whether a release is newer must not treat `0.2.14` as older than
/// `0.2.4`, which string comparison does. Anything richer belongs in the index
/// entry, not in a second version parser.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn core(value: &str) -> [u64; 3] {
        let mut parts = [0u64; 3];
        for (index, part) in value
            .split(['-', '+'])
            .next()
            .unwrap_or(value)
            .split('.')
            .take(3)
            .enumerate()
        {
            parts[index] = part.parse().unwrap_or(0);
        }
        parts
    }
    core(a).cmp(&core(b))
}
