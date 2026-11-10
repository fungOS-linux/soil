//! The one content address a package needs: CIDv1, raw codec, SHA-256.
//!
//! The store never stores, pins or resolves the bytes; an `ipfs://` link in an
//! index entry is a claim that a gateway can hand back exactly these bytes. So
//! the publisher needs to compute the same address an IPFS node would, without
//! depending on one: CIDv1, raw codec (`0x55`), multihash sha2-256 (`0x12 0x20`),
//! multibase base32-lower (`b`).

use sha2::{Digest, Sha256};

/// `ipfs://` address of exactly these bytes.
pub fn raw(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut cid = Vec::with_capacity(4 + digest.len());
    cid.push(0x01); // CIDv1
    cid.push(0x55); // raw codec: the block is the file
    cid.push(0x12); // sha2-256
    cid.push(0x20); // 32 bytes
    cid.extend_from_slice(&digest);
    format!("ipfs://b{}", base32(&cid))
}

/// RFC 4648 base32, lowercase, no padding: what multibase `b` means.
fn base32(bytes: &[u8]) -> String {
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
}
