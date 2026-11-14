//! The one content address Soil uses: CIDv1, raw codec, SHA-256.
//!
//! The store never stores, pins or resolves the bytes; an `ipfs://` link in an
//! index entry is a claim that a gateway can hand back exactly these bytes. So
//! a publisher needs to compute the same address an IPFS node would, without
//! depending on one: CIDv1, raw codec (`0x55`), multihash sha2-256 (`0x12 0x20`),
//! multibase base32-lower (`b`).
//!
//! Sources:
//! - <https://github.com/multiformats/cid>
//! - <https://github.com/multiformats/multibase> (base32, RFC 4648 lowercase)

use sha2::{Digest, Sha256};

/// `ipfs://` address of exactly these bytes.
#[must_use]
pub fn raw(bytes: &[u8]) -> String {
    format!("ipfs://b{}", encode(&address(digest(bytes))))
}

/// The bare multibase string (`b...`) for these bytes, without the `ipfs://`
/// scheme. A host that stores bytes by digest uses this form.
#[must_use]
pub fn bare(bytes: &[u8]) -> String {
    format!("b{}", encode(&address(digest(bytes))))
}

/// The 36 bytes a CIDv1 raw sha2-256 address is: version, codec, multihash.
fn address(digest: [u8; 32]) -> [u8; 36] {
    let mut cid = [0u8; 36];
    cid[0] = 0x01; // CIDv1
    cid[1] = 0x55; // raw codec: the block is the file
    cid[2] = 0x12; // sha2-256
    cid[3] = 0x20; // 32 bytes
    cid[4..].copy_from_slice(&digest);
    cid
}

#[must_use]
pub fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The SHA-256 digest a CIDv1-raw-sha256 address names, from either the
/// `ipfs://b...` form or the bare `b...` form.
pub fn parse(cid: &str) -> Result<[u8; 32], CidError> {
    let body = cid.strip_prefix("ipfs://").unwrap_or(cid);
    let body = body
        .strip_prefix('b')
        .ok_or(CidError::NotBase32Lower)?;
    let bytes = decode(body)?;
    if bytes.len() != 36 {
        return Err(CidError::WrongLength(bytes.len()));
    }
    if bytes[0] != 0x01 {
        return Err(CidError::NotCidV1(bytes[0]));
    }
    if bytes[1] != 0x55 {
        return Err(CidError::NotRawCodec(bytes[1]));
    }
    if bytes[2] != 0x12 || bytes[3] != 0x20 {
        return Err(CidError::NotSha256);
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&bytes[4..]);
    Ok(digest)
}

/// Does `bytes` hash to the address `cid` names?
pub fn verify(cid: &str, bytes: &[u8]) -> Result<(), CidError> {
    let want = parse(cid)?;
    let got = digest(bytes);
    if want == got {
        Ok(())
    } else {
        Err(CidError::Mismatch)
    }
}

/// The hex digest, for a cache that keys files by digest.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest(bytes) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for &byte in bytes {
        accumulator = (accumulator << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((accumulator >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((accumulator << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// RFC 4648 base32, lowercase, no padding.
fn decode(text: &str) -> Result<Vec<u8>, CidError> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for character in text.chars() {
        let value = match character {
            'a'..='z' => character as u32 - 'a' as u32,
            '2'..='7' => character as u32 - '2' as u32 + 26,
            _ => return Err(CidError::NotBase32Lower),
        };
        accumulator = (accumulator << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    // Leftover bits must be zero padding, not data.
    if bits >= 5 || (accumulator & ((1 << bits) - 1)) != 0 {
        return Err(CidError::BadPadding);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidError {
    /// Only multibase base32-lower (`b`) is understood.
    NotBase32Lower,
    /// The base32 body did not decode to whole bytes.
    BadPadding,
    /// A CIDv1-raw-sha256 address is 36 bytes.
    WrongLength(usize),
    NotCidV1(u8),
    /// The raw codec (`0x55`): the block is the file itself.
    NotRawCodec(u8),
    /// Multihash is sha2-256 (`0x12 0x20`).
    NotSha256,
    /// The bytes do not hash to the address.
    Mismatch,
}

impl std::fmt::Display for CidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotBase32Lower => write!(f, "not a base32-lower multibase address"),
            Self::BadPadding => write!(f, "base32 body has non-zero padding"),
            Self::WrongLength(len) => write!(f, "a CIDv1 raw sha2-256 address is 36 bytes, got {len}"),
            Self::NotCidV1(version) => write!(f, "not CIDv1 (version {version})"),
            Self::NotRawCodec(codec) => write!(f, "codec {codec:#04x} is not raw (0x55)"),
            Self::NotSha256 => write!(f, "multihash is not sha2-256"),
            Self::Mismatch => write!(f, "the bytes do not hash to this address"),
        }
    }
}

impl std::error::Error for CidError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_raw_cid_is_a_cidv1_base32_string() {
        let cid = raw(b"a package");
        let body = cid.strip_prefix("ipfs://b").expect("ipfs:// with base32");
        // 36 bytes of CID (v1 + raw + sha2-256 + 32 digest) is 58 base32 chars.
        assert_eq!(body.len(), 58);
        assert!(body.starts_with("afkrei"), "{body}");
        assert!(
            body.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7')),
            "{body}"
        );
    }

    #[test]
    fn the_address_follows_the_bytes_and_nothing_else() {
        assert_eq!(raw(b"one"), raw(b"one"));
        assert_ne!(raw(b"one"), raw(b"two"));
    }

    #[test]
    fn a_bare_address_round_trips_to_its_digest() {
        let digest = digest(b"firmware image");
        let bare = bare(b"firmware image");
        assert!(bare.starts_with('b'));
        assert_eq!(parse(&bare), Ok(digest));
        assert_eq!(parse(&raw(b"firmware image")), Ok(digest));
    }

    #[test]
    fn verification_rejects_bytes_that_do_not_hash_to_the_address() {
        let cid = raw(b"the real image");
        assert_eq!(verify(&cid, b"the real image"), Ok(()));
        assert_eq!(verify(&cid, b"a swapped image"), Err(CidError::Mismatch));
    }

    #[test]
    fn malformed_addresses_are_named_not_guessed() {
        assert_eq!(parse("bnot-a-cid"), Err(CidError::NotBase32Lower));
        assert!(matches!(parse("ipfs://zbad"), Err(CidError::NotBase32Lower)));
        // A valid-length base32 body with the wrong version byte is not guessed.
        assert!(matches!(parse(&format!("b{}", encode(&[0x02; 36]))), Err(CidError::NotCidV1(2))));
        assert!(matches!(parse("b"), Err(CidError::WrongLength(0))));
    }

    #[test]
    fn hex_is_lowercase_and_stable() {
        let hex = hex(b"same");
        assert_eq!(hex.len(), 64);
        assert_eq!(hex, hex_of(b"same"));
    }

    fn hex_of(bytes: &[u8]) -> String {
        let mut out = String::new();
        for byte in digest(bytes) {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}
