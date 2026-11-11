//! A content-addressed blob directory: the bytes behind an `ipfs://` link.
//!
//! The store never invents an address; it keeps exactly the bytes a publisher
//! named and hands them back by CID. Icons, screenshots and clips live here for
//! the same reason packages do: the storefront and an agent should both be able
//! to fetch a picture without a second transport.

use std::path::{Path, PathBuf};

use crate::{Error, Result, cid};

/// Store `bytes` under their CID and return the `ipfs://` address.
pub fn add(dir: &Path, bytes: &[u8]) -> Result<String> {
    let address = cid::raw(bytes);
    std::fs::create_dir_all(dir)
        .map_err(|error| Error::Io(format!("{}: {error}", dir.display())))?;
    let path = path_for(dir, &address);
    std::fs::write(&path, bytes)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    Ok(address)
}

/// Store a file under its CID and return the `ipfs://` address.
pub fn add_file(dir: &Path, file: &Path) -> Result<String> {
    let bytes =
        std::fs::read(file).map_err(|error| Error::Io(format!("{}: {error}", file.display())))?;
    add(dir, &bytes)
}

/// The bytes a CID names, if this directory has them.
///
/// The address is checked before it becomes a path: a request path is not a CID,
/// and `../` in it must not reach the rest of the disk.
pub fn get(dir: &Path, address: &str) -> Option<Vec<u8>> {
    if !addresses(address) {
        return None;
    }
    let path = path_for(dir, address);
    std::fs::read(path).ok()
}

/// Whether this string is a raw CIDv1 in the spelling the store uses.
fn addresses(address: &str) -> bool {
    let body = address.strip_prefix("ipfs://").unwrap_or(address);
    crate::reference(&format!("ipfs://{body}"))
}

/// `<dir>/<cid>`: the address without its `ipfs://` scheme, which is what an
/// HTTP path carries.
fn path_for(dir: &Path, address: &str) -> PathBuf {
    let body = address.strip_prefix("ipfs://").unwrap_or(address);
    dir.join(body)
}

/// The bytes are the name, so the file is written once and never rewritten.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blob_is_addressed_by_its_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let png = b"\x89PNG\r\n\x1a\n fake";
        let address = add(dir.path(), png).expect("add");
        assert!(address.starts_with("ipfs://b"), "{address}");
        assert_eq!(get(dir.path(), &address), Some(png.to_vec()));

        let same = add(dir.path(), png).expect("add again");
        assert_eq!(same, address, "the same bytes are the same address");

        let other = add(dir.path(), b"different").expect("add");
        assert_ne!(other, address);
        assert_eq!(get(dir.path(), &other), Some(b"different".to_vec()));
        assert_eq!(get(dir.path(), "ipfs://bmissing"), None);

        // A request path is not an address, even when the file it names exists.
        std::fs::write(dir.path().join("neighbour"), b"secret").expect("write");
        assert_eq!(get(dir.path(), "ipfs://../neighbour"), None);
        assert_eq!(get(dir.path(), "..%2Fneighbour"), None);
    }
}
