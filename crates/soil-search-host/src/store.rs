//! Small JSON files the host owns about itself.
//!
//! Usage memory and launcher preferences are the same kind of state: this
//! device's, not the mesh's, written at runtime, and never worth failing a
//! summon over. One pair of functions, so the next such file does not grow its
//! own dialect of that policy.

use std::{fs, path::Path};

use serde::{Serialize, de::DeserializeOwned};

/// Read a JSON file. Missing is normal and silent; unreadable is loud and
/// treated as absent, because a host-written cache that cannot be parsed is a
/// cache that was never committed.
pub fn read<T: DeserializeOwned>(path: &Path, label: &str) -> Option<T> {
    let Ok(text) = fs::read_to_string(path) else {
        return None;
    };
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            eprintln!(
                "soil:search: ignoring unreadable {label} at {}: {error}",
                path.display()
            );
            None
        }
    }
}

/// Write beside the target and rename into place: a summon may read this file
/// while another process writes it, and a half-written file is not a file.
pub fn write<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(value)?;
    let temp = path.with_extension("tmp");
    fs::write(&temp, body)?;
    fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Deserialize, serde::Serialize)]
    struct Sample {
        value: u32,
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read::<Sample>(&dir.path().join("absent.json"), "sample").is_none());
    }

    #[test]
    fn writing_creates_parents_and_replaces_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.json");
        write(&path, &Sample { value: 1 }).unwrap();
        write(&path, &Sample { value: 2 }).unwrap();
        assert_eq!(read::<Sample>(&path, "sample"), Some(Sample { value: 2 }));
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn garbage_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"{ not json").unwrap();
        assert!(read::<Sample>(&path, "sample").is_none());
    }
}
