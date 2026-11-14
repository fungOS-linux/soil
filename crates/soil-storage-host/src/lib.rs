//! Host content-addressed cache behind `soil:storage`.
//!
//! `soil:storage` is for the content Soil ships: the storefront publishes a CID
//! for every package, and a Spore reads it back by that address. It is the
//! mirror of `soil:store`, not a workspace — a user's own files belong in
//! `soil:vfs`, where the host decides what is mounted and paths are relative to
//! it. Nothing here accepts a path from a guest.
//!
//! The cache is a flat, sharded directory keyed by SHA-256 digest, and every
//! read re-hashes what it found: a cache that silently returns the wrong bytes
//! is worse than one that misses. Writes verify that the bytes hash to the
//! address they are stored under, so the cache cannot invent an address for
//! content that was never published.
//!
//! This crate is the storage policy, not the capability. Linking it into a host
//! is what makes `soil:storage` callable by a Spore.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use soil_cid as cid;

/// Default ceiling for one object: a firmware image, a watch face, a document.
pub const DEFAULT_MAX_OBJECT_BYTES: u64 = 256 * 1024 * 1024;
/// Default ceiling for the whole cache.
pub const DEFAULT_QUOTA_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Storage {
    root: PathBuf,
    max_object_bytes: u64,
    quota_bytes: u64,
}

impl Storage {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_limits(root, DEFAULT_MAX_OBJECT_BYTES, DEFAULT_QUOTA_BYTES)
    }

    pub fn with_limits(
        root: impl Into<PathBuf>,
        max_object_bytes: u64,
        quota_bytes: u64,
    ) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)
            .with_context(|| format!("create storage root {}", root.display()))?;
        if max_object_bytes == 0 {
            bail!("max_object_bytes must be positive");
        }
        if quota_bytes < max_object_bytes {
            bail!("quota_bytes {quota_bytes} is smaller than one object {max_object_bytes}");
        }
        Ok(Self {
            root,
            max_object_bytes,
            quota_bytes,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read the bytes `cid` names, bounded by `max_bytes`.
    ///
    /// The bytes are re-hashed on the way out. A file that no longer matches
    /// its address is a `corrupt` answer, never a successful read.
    pub fn read(&self, cid: &str, max_bytes: u32) -> Result<Vec<u8>, StorageError> {
        if max_bytes == 0 {
            return Err(StorageError::Invalid("max_bytes must be positive"));
        }
        let digest = cid::parse(cid).map_err(StorageError::InvalidCid)?;
        if self.digest_bytes(digest) > u64::from(max_bytes) {
            return Err(StorageError::OverBudget);
        }
        let path = self.path_for(digest);
        let mut file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StorageError::NotFound)
            }
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        let mut bytes = Vec::new();
        file.by_ref()
            .take(u64::from(max_bytes) + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        if bytes.len() as u64 > u64::from(max_bytes) {
            return Err(StorageError::OverBudget);
        }
        if cid::verify(cid, &bytes).is_err() {
            return Err(StorageError::Corrupt);
        }
        Ok(bytes)
    }

    /// Commit `data` under the address it actually hashes to.
    ///
    /// The claimed CID is verified first: a caller that names the wrong address
    /// gets `InvalidCid`, not a cache entry nobody can find again.
    pub fn write(&self, cid: &str, data: &[u8]) -> Result<(), StorageError> {
        if data.len() as u64 > self.max_object_bytes {
            return Err(StorageError::OverBudget);
        }
        cid::verify(cid, data).map_err(|error| match error {
            cid::CidError::Mismatch => {
                StorageError::Invalid("the bytes do not hash to this address")
            }
            other => StorageError::InvalidCid(other),
        })?;
        let digest = cid::parse(cid).map_err(StorageError::InvalidCid)?;
        let path = self.path_for(digest);
        if path.exists() {
            return Ok(());
        }
        let used = self
            .used_bytes()
            .map_err(|error| StorageError::Io(error.to_string()))?;
        if used.saturating_add(data.len() as u64) > self.quota_bytes {
            return Err(StorageError::OverBudget);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| StorageError::Io(error.to_string()))?;
        }
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temporary, data).map_err(|error| StorageError::Io(error.to_string()))?;
        fs::rename(&temporary, &path).map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(())
    }

    #[must_use]
    pub fn contains(&self, cid: &str) -> bool {
        cid::parse(cid)
            .map(|digest| self.path_for(digest).exists())
            .unwrap_or(false)
    }

    pub fn used_bytes(&self) -> Result<u64> {
        let mut total = 0;
        let shards = match fs::read_dir(&self.root) {
            Ok(shards) => shards,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        for shard in shards.flatten() {
            let Ok(entries) = fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata()
                    && metadata.is_file()
                {
                    total += metadata.len();
                }
            }
        }
        Ok(total)
    }

    fn path_for(&self, digest: [u8; 32]) -> PathBuf {
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        self.root.join(&hex[..2]).join(&hex)
    }

    fn digest_bytes(&self, digest: [u8; 32]) -> u64 {
        self.path_for(digest)
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// The `soil:storage` grant was not linked for this Spore.
    Denied,
    NotFound,
    /// The object, or the write, exceeds a host or caller budget.
    OverBudget,
    /// The address is not a CIDv1 raw sha2-256 address, or does not match.
    InvalidCid(cid::CidError),
    Invalid(&'static str),
    /// The bytes on disk no longer hash to their address.
    Corrupt,
    Io(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied => write!(f, "the soil:storage grant was not linked"),
            Self::NotFound => write!(f, "the cid is not present in the host cache"),
            Self::OverBudget => write!(f, "the object exceeds the host or caller budget"),
            Self::InvalidCid(error) => write!(f, "{error}"),
            Self::Invalid(reason) => write!(f, "{reason}"),
            Self::Corrupt => write!(f, "the cached bytes no longer hash to their address"),
            Self::Io(error) => write!(f, "storage io: {error}"),
        }
    }
}

impl std::error::Error for StorageError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::TempDir::with_prefix("soil-storage").expect("tempdir");
        let storage = Storage::with_limits(dir.path(), 1024, 4096).expect("storage");
        (dir, storage)
    }

    #[test]
    fn bytes_round_trip_through_their_address() {
        let (_dir, storage) = cache();
        let body = b"firmware image bytes";
        let cid = cid::raw(body);
        storage.write(&cid, body).expect("write");
        assert!(storage.contains(&cid));
        assert_eq!(storage.read(&cid, 1024).expect("read"), body);
    }

    #[test]
    fn a_write_under_the_wrong_address_is_refused() {
        let (_dir, storage) = cache();
        let cid = cid::raw(b"one");
        let error = storage.write(&cid, b"two").expect_err("mismatch");
        assert_eq!(error, StorageError::Invalid("the bytes do not hash to this address"));
        assert!(!storage.contains(&cid));
    }

    #[test]
    fn a_missing_address_is_not_found_not_empty() {
        let (_dir, storage) = cache();
        assert_eq!(
            storage.read(&cid::raw(b"absent"), 100),
            Err(StorageError::NotFound)
        );
    }

    #[test]
    fn a_tampered_file_is_corrupt_not_served() {
        let (dir, storage) = cache();
        let cid = cid::raw(b"the real image");
        storage.write(&cid, b"the real image").expect("write");
        let digest = cid::parse(&cid).expect("digest");
        let path = storage.path_for(digest);
        fs::write(&path, b"a swapped image").expect("tamper");
        assert_eq!(storage.read(&cid, 1024), Err(StorageError::Corrupt));
        drop(dir);
    }

    #[test]
    fn caller_and_host_budgets_are_enforced() {
        let (_dir, storage) = cache();
        let body = vec![7u8; 200];
        let cid = cid::raw(&body);
        storage.write(&cid, &body).expect("write");
        assert_eq!(storage.read(&cid, 100), Err(StorageError::OverBudget));

        let huge = vec![9u8; 2000];
        assert_eq!(storage.write(&cid::raw(&huge), &huge), Err(StorageError::OverBudget));
    }
}
