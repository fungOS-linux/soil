//! CID-neutral content identity, local filesystem storage, and the first
//! rebuildable index stub.
//!
//! A CID is a provider locator, not the primary key. `ContentId` is the
//! canonical whole-content identity. The same bytes can be stored in a local
//! CAS, Kubo/IPFS, or iroh-blobs without changing the index record.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use cid::Cid;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

/// A canonical content identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContentId {
    /// `blake3` today; the field is explicit so providers can migrate.
    pub algorithm: String,
    /// Lowercase hexadecimal digest.
    pub digest: String,
}

impl ContentId {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let digest = blake3::hash(bytes);
        Self {
            algorithm: "blake3".into(),
            digest: digest.to_hex().to_string(),
        }
    }

    pub fn parse(value: &str) -> Result<Self, ContentError> {
        let (algorithm, digest) = value
            .split_once(':')
            .ok_or_else(|| ContentError::InvalidContentId(value.into()))?;
        if algorithm.is_empty()
            || digest.len() != 64
            || !digest.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(ContentError::InvalidContentId(value.into()));
        }
        Ok(Self {
            algorithm: algorithm.into(),
            digest: digest.to_ascii_lowercase(),
        })
    }
}

impl std::fmt::Display for ContentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.digest)
    }
}

/// A provider-specific address. A `BlobRef` may carry several at once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locator {
    Local { key: String },
    Ipfs { cid: String },
    Iroh { hash: String },
    S3 { bucket: String, key: String },
}

impl Locator {
    pub fn scheme(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::Ipfs { .. } => "ipfs",
            Self::Iroh { .. } => "iroh",
            Self::S3 { .. } => "s3",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub content_id: ContentId,
    pub size: u64,
    pub media_type: String,
    pub locators: Vec<Locator>,
}

impl BlobRef {
    pub fn new(content_id: ContentId, size: u64, media_type: impl Into<String>) -> Self {
        Self {
            content_id,
            size,
            media_type: media_type.into(),
            locators: Vec::new(),
        }
    }

    pub fn add_locator(&mut self, locator: Locator) -> &mut Self {
        if !self.locators.contains(&locator) {
            self.locators.push(locator);
        }
        self
    }

    pub fn locator(&self, scheme: &str) -> Option<&Locator> {
        self.locators
            .iter()
            .find(|locator| locator.scheme() == scheme)
    }
}

#[derive(Debug, Error)]
pub enum ContentError {
    #[error("invalid content id: {0}")]
    InvalidContentId(String),
    #[error("blob not found: {0}")]
    NotFound(String),
    #[error("invalid cid: {0}")]
    InvalidCid(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub trait BlobProvider {
    fn scheme(&self) -> &'static str;
    fn put(&self, media_type: &str, bytes: &[u8]) -> Result<Locator, ContentError>;
    fn get(&self, locator: &Locator) -> Result<Vec<u8>, ContentError>;
}

/// A content-addressed local blob store.
pub struct LocalFsProvider {
    root: PathBuf,
}

impl LocalFsProvider {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ContentError> {
        let root = root.into();
        fs::create_dir_all(root.join("blobs"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn key(content_id: &ContentId) -> String {
        format!("blobs/{}/{}", &content_id.digest[..2], content_id.digest)
    }

    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}

impl BlobProvider for LocalFsProvider {
    fn scheme(&self) -> &'static str {
        "local"
    }

    fn put(&self, _media_type: &str, bytes: &[u8]) -> Result<Locator, ContentError> {
        let content_id = ContentId::from_bytes(bytes);
        let key = Self::key(&content_id);
        let path = self.path(&key);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
            fs::write(&temporary, bytes)?;
            fs::rename(temporary, &path)?;
        }
        Ok(Locator::Local { key })
    }

    fn get(&self, locator: &Locator) -> Result<Vec<u8>, ContentError> {
        let Locator::Local { key } = locator else {
            return Err(ContentError::Provider(format!(
                "local provider cannot read {}",
                locator.scheme()
            )));
        };
        let path = self.path(key);
        fs::read(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ContentError::NotFound(key.clone())
            } else {
                ContentError::Io(error)
            }
        })
    }
}

/// Placeholder for Kubo/IPFS. It creates real raw-block CIDv1 locators but
/// keeps bytes in memory until the HTTP RPC adapter lands.
#[derive(Default)]
pub struct IpfsStub {
    blocks: Mutex<BTreeMap<ContentId, Vec<u8>>>,
    cid_to_content: Mutex<BTreeMap<String, ContentId>>,
}

impl IpfsStub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn content_for_cid(&self, cid: &str) -> Option<ContentId> {
        self.cid_to_content
            .lock()
            .expect("ipfs stub poisoned")
            .get(cid)
            .cloned()
    }
}

impl BlobProvider for IpfsStub {
    fn scheme(&self) -> &'static str {
        "ipfs"
    }

    fn put(&self, _media_type: &str, bytes: &[u8]) -> Result<Locator, ContentError> {
        let content_id = ContentId::from_bytes(bytes);
        let digest = Sha256::digest(bytes);
        let hash = cid::multihash::Multihash::wrap(0x12, &digest)
            .map_err(|error| ContentError::InvalidCid(error.to_string()))?;
        let cid = Cid::new_v1(0x55, hash).to_string();
        self.blocks
            .lock()
            .expect("ipfs stub poisoned")
            .insert(content_id.clone(), bytes.to_vec());
        self.cid_to_content
            .lock()
            .expect("ipfs stub poisoned")
            .insert(cid.clone(), content_id);
        Ok(Locator::Ipfs { cid })
    }

    fn get(&self, locator: &Locator) -> Result<Vec<u8>, ContentError> {
        let Locator::Ipfs { cid } = locator else {
            return Err(ContentError::Provider(format!(
                "ipfs provider cannot read {}",
                locator.scheme()
            )));
        };
        let content_id = self
            .content_for_cid(cid)
            .ok_or_else(|| ContentError::NotFound(cid.clone()))?;
        self.blocks
            .lock()
            .expect("ipfs stub poisoned")
            .get(&content_id)
            .cloned()
            .ok_or_else(|| ContentError::NotFound(cid.clone()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexEntry {
    pub entity_id: String,
    pub blob: BlobRef,
    pub text: String,
    pub metadata: BTreeMap<String, String>,
}

/// A set of pluggable providers used to place and resolve one content id.
pub struct ContentStore {
    providers: Vec<Box<dyn BlobProvider>>,
}

impl ContentStore {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn add_provider(&mut self, provider: Box<dyn BlobProvider>) -> &mut Self {
        self.providers.push(provider);
        self
    }

    pub fn put(&self, media_type: &str, bytes: &[u8]) -> Result<BlobRef, ContentError> {
        let mut blob = BlobRef::new(ContentId::from_bytes(bytes), bytes.len() as u64, media_type);
        for provider in &self.providers {
            if let Ok(locator) = provider.put(media_type, bytes) {
                blob.add_locator(locator);
            }
        }
        if blob.locators.is_empty() {
            return Err(ContentError::Provider(
                "no provider accepted the blob".into(),
            ));
        }
        Ok(blob)
    }

    pub fn get(&self, blob: &BlobRef) -> Result<Vec<u8>, ContentError> {
        let mut last_error = None;
        for locator in &blob.locators {
            let Some(provider) = self
                .providers
                .iter()
                .find(|provider| provider.scheme() == locator.scheme())
            else {
                continue;
            };
            match provider.get(locator) {
                Ok(bytes) if ContentId::from_bytes(&bytes) == blob.content_id => return Ok(bytes),
                Ok(_) => {
                    last_error = Some(ContentError::Provider(format!(
                        "{} returned bytes for the wrong content id",
                        locator.scheme()
                    )))
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| ContentError::NotFound(blob.content_id.to_string())))
    }
}

impl Default for ContentStore {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ContentIndex {
    entries: BTreeMap<String, IndexEntry>,
    by_content: BTreeMap<ContentId, Vec<String>>,
    by_locator: BTreeMap<String, String>,
}

impl Default for ContentIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentIndex {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            by_content: BTreeMap::new(),
            by_locator: BTreeMap::new(),
        }
    }

    pub fn upsert(&mut self, entry: IndexEntry) {
        let entity_id = entry.entity_id.clone();
        if let Some(previous) = self.entries.insert(entity_id.clone(), entry.clone()) {
            if let Some(ids) = self.by_content.get_mut(&previous.blob.content_id) {
                ids.retain(|id| id != &entity_id);
            }
            for locator in previous.blob.locators {
                if let Some(cid) = locator_string(&locator) {
                    self.by_locator.remove(&cid);
                }
            }
        }
        self.by_content
            .entry(entry.blob.content_id.clone())
            .or_default()
            .push(entity_id.clone());
        for locator in &entry.blob.locators {
            if let Some(value) = locator_string(locator) {
                self.by_locator.insert(value, entity_id.clone());
            }
        }
    }

    pub fn by_entity(&self, entity_id: &str) -> Option<&IndexEntry> {
        self.entries.get(entity_id)
    }

    pub fn by_content(&self, content_id: &ContentId) -> impl Iterator<Item = &IndexEntry> {
        self.by_content
            .get(content_id)
            .into_iter()
            .flatten()
            .filter_map(|id| self.entries.get(id))
    }

    /// Resolve an IPFS CID or Iroh hash to the entity that owns it.
    pub fn by_locator(&self, value: &str) -> Option<&IndexEntry> {
        self.by_locator
            .get(value)
            .and_then(|entity_id| self.entries.get(entity_id))
    }

    /// Minimal lexical stub; the real index will add BM25 + vectors.
    pub fn search(&self, query: &str) -> Vec<&IndexEntry> {
        let query = query.to_ascii_lowercase();
        self.entries
            .values()
            .filter(|entry| {
                entry.entity_id.to_ascii_lowercase().contains(&query)
                    || entry.text.to_ascii_lowercase().contains(&query)
                    || entry
                        .metadata
                        .values()
                        .any(|value| value.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    pub fn save(&self, path: &Path) -> Result<(), ContentError> {
        let entries: Vec<_> = self.entries.values().cloned().collect();
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&temporary, serde_json::to_vec_pretty(&entries)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, ContentError> {
        let entries: Vec<IndexEntry> = serde_json::from_slice(&fs::read(path)?)?;
        let mut index = Self::new();
        for entry in entries {
            index.upsert(entry);
        }
        Ok(index)
    }
}

fn locator_string(locator: &Locator) -> Option<String> {
    match locator {
        Locator::Ipfs { cid } => Some(cid.clone()),
        Locator::Iroh { hash } => Some(hash.clone()),
        Locator::S3 { bucket, key } => Some(format!("s3://{bucket}/{key}")),
        Locator::Local { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_store_and_index_round_trip() {
        let root = std::env::temp_dir().join(format!(
            "fungos-content-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let local = LocalFsProvider::new(&root).unwrap();
        let bytes = b"hello content";
        let locator = local.put("text/plain", bytes).unwrap();
        assert_eq!(local.get(&locator).unwrap(), bytes);
        let ipfs = IpfsStub::new();
        let ipfs_locator = ipfs.put("text/plain", bytes).unwrap();
        let Locator::Ipfs { cid } = &ipfs_locator else {
            panic!("expected ipfs locator")
        };
        let cid = cid.clone();

        let mut blob = BlobRef::new(
            ContentId::from_bytes(bytes),
            bytes.len() as u64,
            "text/plain",
        );
        blob.add_locator(locator);
        blob.add_locator(ipfs_locator);
        let mut index = ContentIndex::new();
        index.upsert(IndexEntry {
            entity_id: "doc-1".into(),
            blob,
            text: "hello searchable world".into(),
            metadata: BTreeMap::from([("kind".into(), "document".into())]),
        });
        assert_eq!(index.search("searchable").len(), 1);
        assert_eq!(index.search("doc-1").len(), 1);
        assert_eq!(
            index.by_locator(&cid).map(|entry| &entry.entity_id),
            Some(&"doc-1".to_string())
        );
        assert_eq!(
            ipfs.content_for_cid(cid.as_str()),
            Some(ContentId::from_bytes(bytes))
        );
        let path = root.join("index.json");
        index.save(&path).unwrap();
        let loaded = ContentIndex::load(&path).unwrap();
        assert_eq!(loaded.search("hello").len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn content_store_places_one_content_id_across_providers() {
        let root = std::env::temp_dir().join(format!(
            "fungos-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = ContentStore::new();
        store.add_provider(Box::new(LocalFsProvider::new(&root).unwrap()));
        store.add_provider(Box::new(IpfsStub::new()));
        let bytes = b"same content, two providers";
        let blob = store.put("text/plain", bytes).unwrap();
        assert_eq!(blob.content_id, ContentId::from_bytes(bytes));
        assert_eq!(blob.locators.len(), 2);
        assert_eq!(store.get(&blob).unwrap(), bytes);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ipfs_stub_emits_a_cid_and_resolves_back_to_content() {
        let ipfs = IpfsStub::new();
        let bytes = b"cid-neutral bytes";
        let locator = ipfs.put("application/octet-stream", bytes).unwrap();
        let Locator::Ipfs { cid } = &locator else {
            panic!("expected ipfs locator")
        };
        assert!(Cid::try_from(cid.as_str()).is_ok());
        assert_eq!(
            ipfs.content_for_cid(cid.as_str()),
            Some(ContentId::from_bytes(bytes))
        );
        assert_eq!(ipfs.get(&locator).unwrap(), bytes);
    }
}
