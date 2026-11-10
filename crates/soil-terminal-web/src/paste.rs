//! Content-addressed paste store.
//!
//! A pasted file is written once under its SHA-256 and the terminal receives
//! its path. Pasting the same bytes twice reuses the same file, so a screenshot
//! dropped into an agent costs one write and one short path, not a copy per
//! paste.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub struct Paste {
    root: PathBuf,
}

pub struct Stored {
    pub cid: String,
    pub path: PathBuf,
    pub name: String,
    pub bytes: usize,
    pub media_type: String,
}

impl Stored {
    pub fn to_json(&self) -> Value {
        json!({
            "cid": self.cid,
            "path": self.path.display().to_string(),
            "name": self.name,
            "bytes": self.bytes,
            "media_type": self.media_type,
        })
    }
}

impl Paste {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn store(&self, bytes: &[u8], name: &str, media_type: &str) -> Result<Stored> {
        let digest = Sha256::digest(bytes);
        let cid = hex(digest.as_slice());
        let name = sanitize_name(name, media_type);
        let directory = self.root.join(&cid);
        fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join(&name);
        if !path.exists() {
            fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        }
        Ok(Stored {
            cid,
            path,
            name,
            bytes: bytes.len(),
            media_type: media_type.to_owned(),
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Keep one path segment: no separators, no control characters, no traversal.
fn sanitize_name(name: &str, media_type: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("paste");
    let mut cleaned: String = base
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                ch
            }
        })
        .take(128)
        .collect();
    if name.trim().is_empty() {
        cleaned = format!("paste{}", extension(media_type));
    }
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        cleaned = format!("paste{}", extension(media_type));
    }
    cleaned
}

fn extension(media_type: &str) -> &'static str {
    match media_type.split(';').next().unwrap_or("").trim() {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/svg+xml" => ".svg",
        "text/plain" => ".txt",
        "text/markdown" => ".md",
        "application/json" => ".json",
        "application/pdf" => ".pdf",
        _ => ".bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_bytes_share_one_cid() {
        let directory = tempfile::tempdir().unwrap();
        let paste = Paste::open(directory.path().to_path_buf()).unwrap();
        let first = paste.store(b"hello", "shot.png", "image/png").unwrap();
        let second = paste.store(b"hello", "other.png", "image/png").unwrap();
        assert_eq!(first.cid, second.cid);
        assert_eq!(first.cid.len(), 64);
        assert!(first.path.starts_with(directory.path()));
        assert!(first.path.exists());
        assert_eq!(first.path.file_name().unwrap(), "shot.png");
        assert_eq!(second.path.file_name().unwrap(), "other.png");
    }

    #[test]
    fn names_are_sanitized_and_defaulted() {
        let directory = tempfile::tempdir().unwrap();
        let paste = Paste::open(directory.path().to_path_buf()).unwrap();
        let stored = paste.store(b"x", "../../etc/passwd", "image/png").unwrap();
        assert_eq!(stored.name, "passwd");
        assert!(stored.path.starts_with(directory.path()));
        let unnamed = paste.store(b"y", "", "image/png").unwrap();
        assert_eq!(unnamed.name, "paste.png");
    }
}
