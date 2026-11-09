//! Shared frames for GPU lease pixels.
//!
//! The lease metadata still travels as JSON on `ui.sock`; the pixels travel in
//! a producer-owned file under `/tmp`. The producer creates one file per frame,
//! writes a small header and the tightly packed RGBA bytes, and sends the path.
//! The Canvas reader opens it, validates the header, copies the pixels, and
//! removes the path.
//!
//! This replaces base64 `gpu_frame` payloads without changing the lease
//! contract. It is file-backed rather than POSIX `shm_open` because macOS shm
//! names are both length-limited and unreliable in sandboxed dev runs; the
//! transport shape is the same, and a memfd/ring can replace the file later
//! without changing the message or lease.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"FUNGOSF1";
pub const VERSION: u32 = 1;
pub const HEADER_BYTES: usize = 4096;
const MAX_DIMENSION: u32 = 4096;
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const FRAME_PREFIX: &str = "fungos-frame-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFrame {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFrameData {
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub rgba: Vec<u8>,
}

/// Publish one tightly packed RGBA frame into a new shared frame file.
pub fn publish(width: u32, height: u32, revision: u64, rgba: &[u8]) -> io::Result<SharedFrame> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared frame extent is outside 1..=4096",
        ));
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame extent overflows"))?;
    if expected != rgba.len() || expected > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared frame length does not match its extent",
        ));
    }

    let (path, mut file) = create_unique()?;
    let mut header = vec![0u8; HEADER_BYTES];
    header[0..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&VERSION.to_le_bytes());
    header[12..16].copy_from_slice(&width.to_le_bytes());
    header[16..20].copy_from_slice(&height.to_le_bytes());
    header[20..24].copy_from_slice(&(width * 4).to_le_bytes());
    header[24..32].copy_from_slice(&revision.to_le_bytes());
    header[32..40].copy_from_slice(&revision.to_le_bytes());
    file.write_all(&header)?;
    file.write_all(rgba)?;
    file.flush()?;
    Ok(SharedFrame {
        name: path.to_string_lossy().into_owned(),
        width,
        height,
        revision,
    })
}

/// Read one published frame and remove its path.
pub fn read(name: &str) -> io::Result<SharedFrameData> {
    let path = Path::new(name);
    validate_path(path)?;
    let mut file = File::open(path)?;
    let mut header = vec![0u8; HEADER_BYTES];
    file.read_exact(&mut header)?;
    let result = parse_header(&header).and_then(|(width, height, revision)| {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame extent overflows"))?;
        let mut rgba = vec![0u8; expected];
        file.seek(SeekFrom::Start(HEADER_BYTES as u64))?;
        file.read_exact(&mut rgba)?;
        Ok(SharedFrameData {
            width,
            height,
            revision,
            rgba,
        })
    });
    let _ = fs::remove_file(path);
    result
}

fn create_unique() -> io::Result<(PathBuf, File)> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = Path::new("/tmp");
    for _ in 0..32 {
        let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = root.join(format!("{FRAME_PREFIX}{}-{counter}", std::process::id()));
        match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a shared frame name",
    ))
}

fn validate_path(path: &Path) -> io::Result<()> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared frame has no file name",
        ));
    };
    if path.parent() != Some(Path::new("/tmp"))
        || !name.starts_with(FRAME_PREFIX)
        || name.contains('/')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared frame path is outside the shared frame directory",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared frame is not a regular file",
        ));
    }
    Ok(())
}

fn parse_header(map: &[u8]) -> io::Result<(u32, u32, u64)> {
    if map.len() < 40 || &map[0..8] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared frame header is not fungos",
        ));
    }
    let version = u32::from_le_bytes(map[8..12].try_into().unwrap());
    if version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared frame version is unsupported",
        ));
    }
    let width = u32::from_le_bytes(map[12..16].try_into().unwrap());
    let height = u32::from_le_bytes(map[16..20].try_into().unwrap());
    let stride = u32::from_le_bytes(map[20..24].try_into().unwrap());
    let revision = u64::from_le_bytes(map[24..32].try_into().unwrap());
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || stride != width * 4
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared frame extent or stride is invalid",
        ));
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame extent overflows"))?;
    if expected > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared frame is over the byte ceiling",
        ));
    }
    Ok((width, height, revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_roundtrips_and_is_removed_by_the_reader() {
        let rgba = [1u8, 2, 3, 255, 4, 5, 6, 255];
        let frame = publish(2, 1, 7, &rgba).expect("publish");
        let data = read(&frame.name).expect("read");
        assert_eq!(data.width, 2);
        assert_eq!(data.height, 1);
        assert_eq!(data.revision, 7);
        assert_eq!(data.rgba, rgba);
        assert!(!Path::new(&frame.name).exists(), "reader removes the frame");
    }

    #[test]
    fn mismatched_pixels_are_refused() {
        assert!(publish(2, 2, 1, &[0, 0, 0, 255]).is_err());
    }
}
