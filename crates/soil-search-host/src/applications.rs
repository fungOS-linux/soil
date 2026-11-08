//! Installed applications, discovered the way a launcher host sees them.
//!
//! This is the dynamic half of the application source. `discovery::ApplicationIndex`
//! also loads a snapshot, for hosts that already know their inventory, but a
//! launcher that ships a static list is a launcher that lies about the machine.
//! Scanning is bounded (one directory level, one Info.plist per bundle) and
//! executes nothing.
//!
//! Icons stay a locator here: [`icon_png`] returns the bundle's own PNG bytes and
//! the HTTP surface hands them to a renderer. `soil:search` reports an asset
//! reference, not bytes, so the bytes never enter a hit.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const MAX_APPLICATIONS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub bundle_id: String,
    pub name: String,
    /// The folder the bundle sits in: what a person calls where it lives.
    pub folder: String,
    pub bundle: PathBuf,
    pub icns: Option<PathBuf>,
}

/// Where this platform installs bundles. Linux desktop entries are a different
/// format and a different walk; they are not pretend-macOS here.
pub fn bundle_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots
}

/// Every bundle under `roots`, deduplicated by bundle id and ordered by name.
///
/// A missing or unreadable bundle is skipped rather than propagated: an
/// inventory that cannot be read is not a search that cannot run.
pub fn scan(roots: &[PathBuf]) -> Vec<InstalledApp> {
    let mut found: BTreeMap<String, InstalledApp> = BTreeMap::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let bundle = entry.path();
            if bundle.extension().and_then(|ext| ext.to_str()) != Some("app") {
                continue;
            }
            if let Some(app) = read_bundle(&bundle) {
                found.entry(app.bundle_id.clone()).or_insert(app);
            }
            if found.len() >= MAX_APPLICATIONS {
                break;
            }
        }
    }
    let mut apps: Vec<InstalledApp> = found.into_values().collect();
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.bundle_id.cmp(&b.bundle_id))
    });
    apps
}

/// Read one bundle's `Info.plist`. No recursive walk, no icon decoding yet.
pub fn read_bundle(bundle: &Path) -> Option<InstalledApp> {
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    let info = info.into_dictionary()?;
    let text = |key: &str| {
        info.get(key)
            .and_then(|value| value.as_string())
            .map(str::to_owned)
    };
    let bundle_id = text("CFBundleIdentifier")?;
    let name = text("CFBundleDisplayName")
        .or_else(|| text("CFBundleName"))
        .or_else(|| {
            bundle
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
        })?;
    // `CFBundleIconName` names an asset-catalog icon, which has no file to read;
    // it is tried last so a bundle with a real `.icns` still gets its artwork.
    let icns = [text("CFBundleIconFile"), text("CFBundleIconName")]
        .into_iter()
        .flatten()
        .map(|name| {
            let file = if name.ends_with(".icns") {
                name
            } else {
                format!("{name}.icns")
            };
            bundle.join("Contents/Resources").join(file)
        })
        .find(|path| path.exists());
    let folder = bundle
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("Applications")
        .to_owned();
    Some(InstalledApp {
        bundle_id,
        name,
        folder,
        bundle: bundle.to_path_buf(),
        icns,
    })
}

/// The bundle's own icon as PNG bytes, when its container holds a PNG.
///
/// An `.icns` is a chunked container and every current macOS icon stores at least
/// one PNG in it, so this reads the container instead of decoding the historical
/// ARGB formats or shelling out to a converter.
pub fn icon_png(app: &InstalledApp) -> Option<Vec<u8>> {
    let bytes = fs::read(app.icns.as_ref()?).ok()?;
    png_from_icns(&bytes)
}

/// Pick the most useful PNG out of an ICNS container: the smallest that is still
/// crisp at launcher size, and if none is that large, the largest there is.
pub fn png_from_icns(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 8 || &bytes[0..4] != b"icns" {
        return None;
    }
    let declared = u32::from_be_bytes(bytes[4..8].try_into().ok()?) as usize;
    let end = declared.clamp(8, bytes.len());
    let mut best: Option<((i32, i32), Vec<u8>)> = None;
    let mut offset = 8;
    while offset + 8 <= end {
        let length = u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into().ok()?) as usize;
        if length < 8 || offset + length > end {
            break;
        }
        let payload = &bytes[offset + 8..offset + length];
        if payload.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
            let width = payload
                .get(16..20)
                .and_then(|slice| slice.try_into().ok())
                .map(u32::from_be_bytes)
                .unwrap_or(0);
            // Bucket 0 is "128 or larger", where smaller wins; bucket 1 is
            // smaller than any launcher draws, where larger wins.
            let rank = if width >= 128 {
                (0, width as i32)
            } else {
                (1, -(width as i32))
            };
            if best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank) {
                best = Some((rank, payload.to_vec()));
            }
        }
        offset += length;
    }
    best.map(|(_, png)| png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&13u32.to_be_bytes()); // IHDR length
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn icns(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (kind, payload) in chunks {
            body.extend_from_slice(*kind);
            body.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
            body.extend_from_slice(payload);
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"icns");
        bytes.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        bytes.extend_from_slice(&body);
        bytes
    }

    #[test]
    fn picks_the_smallest_png_that_is_still_crisp() {
        let container = icns(&[
            (b"is32", vec![0u8; 16]), // legacy ARGB: no PNG, no use
            (b"ic07", png(128)),
            (b"ic08", png(256)),
        ]);
        let chosen = png_from_icns(&container).unwrap();
        assert_eq!(u32::from_be_bytes(chosen[16..20].try_into().unwrap()), 128);
    }

    #[test]
    fn falls_back_to_the_largest_when_nothing_is_launcher_sized() {
        let container = icns(&[(b"ic11", png(32)), (b"ic12", png(64))]);
        let chosen = png_from_icns(&container).unwrap();
        assert_eq!(u32::from_be_bytes(chosen[16..20].try_into().unwrap()), 64);
    }

    #[test]
    fn a_container_without_pngs_is_not_artwork() {
        assert!(png_from_icns(&icns(&[(b"is32", vec![0u8; 16])])).is_none());
        assert!(png_from_icns(b"not an icon").is_none());
        assert!(png_from_icns(&[]).is_none());
    }

    #[test]
    fn a_bundle_without_a_manifest_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Broken.app");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        assert!(read_bundle(&bundle).is_none());
    }
}
