//! Publisher signatures over a Spore package.
//!
//! The package leaf is signed by a delegated publisher key. A host-supplied
//! [`PublisherTrust`] verifies that the key is still authorized for the Spore
//! and publisher. Soil deliberately does not own the authorization table;
//! Mycelium supplies the signed grant/revocation view.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Lock, Manifest, Result};

pub const PUBLISHER_SCHEMA: &str = "fungos.spore-publisher/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub spore_id: String,
    pub version: String,
    pub package_digest: String,
    pub principal: String,
    pub grant_id: String,
    pub serial: u64,
    /// Hex-encoded ed25519 public key of the delegated publisher.
    pub publisher_key: String,
    pub not_before: u64,
    pub not_after: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema: String,
    pub statement: Statement,
    /// Hex-encoded ed25519 signature over canonical statement JSON.
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedPublisher {
    pub principal: String,
    pub grant_id: String,
    pub key_id: String,
}

/// Host integration point. A Mycelium-backed implementation verifies the
/// signed publisher grant, delegation and revocation here. The trait must fail
/// closed: `Ok` means this exact statement is authorized.
pub trait PublisherTrust {
    fn authorize(&self, statement: &Statement, now_unix_s: u64) -> std::result::Result<(), String>;
}

/// Host allowlist for system Spores. The manifest's `[system]` flag is only a
/// declaration; this table is the host's actual trust decision.
#[derive(Debug, Default, Clone)]
pub struct SystemAllowlist {
    entries: std::collections::BTreeMap<String, SystemEntry>,
}

#[derive(Debug, Clone)]
struct SystemEntry {
    role: String,
    principal: String,
    digest: String,
}

impl SystemAllowlist {
    pub fn allow(
        &mut self,
        spore_id: impl Into<String>,
        role: impl Into<String>,
        principal: impl Into<String>,
        package_digest: impl Into<String>,
    ) {
        self.entries.insert(
            spore_id.into(),
            SystemEntry {
                role: role.into(),
                principal: principal.into(),
                digest: package_digest.into(),
            },
        );
    }

    pub fn authorize(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        publisher: &TrustedPublisher,
    ) -> Result<()> {
        let Some(system) = &manifest.system else {
            return Err(Error::Signature("Spore is not a system Spore".into()));
        };
        let Some(entry) = self.entries.get(&manifest.spore.id) else {
            return Err(Error::Signature("system Spore is not allowlisted".into()));
        };
        if entry.role != system.role {
            return Err(Error::Signature("system Spore role mismatch".into()));
        }
        if entry.principal != publisher.principal {
            return Err(Error::Signature(
                "system Spore publisher is not allowlisted".into(),
            ));
        }
        if entry.digest != lock.digest {
            return Err(Error::Signature(
                "system Spore package digest is not allowlisted".into(),
            ));
        }
        Ok(())
    }
}

pub fn key_id(public_key_hex: &str) -> Result<String> {
    let bytes = decode(public_key_hex).map_err(Error::Signature)?;
    if bytes.len() != 32 {
        return Err(Error::Signature("publisher key is not 32 bytes".into()));
    }
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn statement_bytes(statement: &Statement) -> Result<Vec<u8>> {
    serde_json::to_vec(statement).map_err(|error| Error::Signature(error.to_string()))
}

pub fn sign(statement: Statement, key: &ed25519_dalek::SigningKey) -> Result<Envelope> {
    use ed25519_dalek::Signer;
    let expected = key_id(&hex(key.verifying_key().as_bytes()))?;
    if expected != key_id(&statement.publisher_key)? {
        return Err(Error::Signature(
            "statement publisher_key does not match signing key".into(),
        ));
    }
    let signature = key.sign(&statement_bytes(&statement)?);
    Ok(Envelope {
        schema: PUBLISHER_SCHEMA.into(),
        statement,
        signature: hex(&signature.to_bytes()),
    })
}

/// Verify the leaf signature and bind it to this exact manifest and lock. The
/// trust provider is responsible for grant authorization, delegation and
/// revocation; this function never trusts the statement by itself.
pub fn verify(
    envelope: &Envelope,
    manifest: &Manifest,
    lock: &Lock,
    trust: &dyn PublisherTrust,
    now_unix_s: u64,
) -> Result<TrustedPublisher> {
    if envelope.schema != PUBLISHER_SCHEMA {
        return Err(Error::Signature(format!(
            "unsupported publisher schema {:?}",
            envelope.schema
        )));
    }
    let statement = &envelope.statement;
    if statement.spore_id != manifest.spore.id
        || statement.version != manifest.spore.version
        || statement.package_digest != lock.digest
    {
        return Err(Error::Signature(
            "publisher statement does not match this package".into(),
        ));
    }
    if statement.not_before > now_unix_s || statement.not_after <= now_unix_s {
        return Err(Error::Signature(
            "publisher statement is not current".into(),
        ));
    }
    let key_bytes = decode(&statement.publisher_key).map_err(Error::Signature)?;
    let key_array: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::Signature("publisher key is not 32 bytes".into()))?;
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&key_array)
        .map_err(|error| Error::Signature(error.to_string()))?;
    let signature_bytes = decode(&envelope.signature).map_err(Error::Signature)?;
    let signature_array: [u8; 64] = signature_bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::Signature("publisher signature is not 64 bytes".into()))?;
    let signature = ed25519_dalek::Signature::from_bytes(&signature_array);
    use ed25519_dalek::Verifier;
    verifying_key
        .verify(&statement_bytes(statement)?, &signature)
        .map_err(|error| Error::Signature(error.to_string()))?;

    trust
        .authorize(statement, now_unix_s)
        .map_err(Error::Signature)?;

    Ok(TrustedPublisher {
        principal: statement.principal.clone(),
        grant_id: statement.grant_id.clone(),
        key_id: key_id(&statement.publisher_key)?,
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn decode(value: &str) -> std::result::Result<Vec<u8>, String> {
    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("hex value has odd length".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    // Stepped indices rather than `chunks_exact`: the pair is the unit here, and
    // an index that is always in range says that without a chunk type.
    for index in (0..bytes.len()).step_by(2) {
        let high = nibble(bytes[index])?;
        let low = nibble(bytes[index + 1])?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn nibble(byte: u8) -> std::result::Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("invalid hex".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Manifest;

    struct Trust {
        deny: bool,
    }
    impl PublisherTrust for Trust {
        fn authorize(&self, _: &Statement, _: u64) -> std::result::Result<(), String> {
            if self.deny {
                Err("revoked".into())
            } else {
                Ok(())
            }
        }
    }

    fn manifest() -> Manifest {
        Manifest::parse(
            r#"
schema = 1
[spore]
id = "dev.fungos.test"
name = "test"
version = "0.1.0"
license = "MIT"
summary = "test"
[engine]
soil = ">=0.1"
target = "wasm32-wasip2"
[module]
path = "module.wasm"
[ui]
entry = "ui/test.lua"
[launch]
default = "native"
surfaces = ["native"]
"#,
        )
        .unwrap()
    }

    fn lock(manifest: &Manifest) -> Lock {
        Lock::new(&manifest.spore.id, &manifest.spore.version, vec![])
    }

    fn envelope() -> Envelope {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let manifest = manifest();
        let lock = lock(&manifest);
        sign(
            Statement {
                spore_id: manifest.spore.id,
                version: manifest.spore.version,
                package_digest: lock.digest,
                principal: "oidc:example#avery".into(),
                grant_id: "grant-1".into(),
                serial: 1,
                publisher_key: hex(key.verifying_key().as_bytes()),
                not_before: 10,
                not_after: 100,
            },
            &key,
        )
        .unwrap()
    }

    #[test]
    fn binds_signature_to_package_and_trust() {
        let envelope = envelope();
        let manifest = manifest();
        let lock = lock(&manifest);
        let trusted = verify(&envelope, &manifest, &lock, &Trust { deny: false }, 50).unwrap();
        assert_eq!(trusted.grant_id, "grant-1");
        assert!(verify(&envelope, &manifest, &lock, &Trust { deny: true }, 50).is_err());
        assert!(verify(&envelope, &manifest, &lock, &Trust { deny: false }, 101).is_err());
    }

    #[test]
    fn system_allowlist_requires_exact_id_role_principal_and_digest() {
        let envelope = envelope();
        let mut manifest = manifest();
        manifest.system = Some(crate::System {
            role: "consent".into(),
        });
        let lock = lock(&manifest);
        let trusted = verify(&envelope, &manifest, &lock, &Trust { deny: false }, 50).unwrap();
        let mut allow = SystemAllowlist::default();
        allow.allow(
            manifest.spore.id.clone(),
            "consent",
            "oidc:example#avery",
            lock.digest.clone(),
        );
        assert!(allow.authorize(&manifest, &lock, &trusted).is_ok());
        let mut wrong = allow.clone();
        wrong.allow(
            manifest.spore.id.clone(),
            "consent",
            "oidc:example#avery",
            "0".repeat(64),
        );
        assert!(wrong.authorize(&manifest, &lock, &trusted).is_err());
    }

    #[test]
    fn rejects_tampered_digest_and_key_mismatch() {
        let mut envelope = envelope();
        envelope.statement.package_digest = "0".repeat(64);
        let manifest = manifest();
        let lock = lock(&manifest);
        assert!(verify(&envelope, &manifest, &lock, &Trust { deny: false }, 50).is_err());

        let key = ed25519_dalek::SigningKey::from_bytes(&[8u8; 32]);
        let mut statement = envelope.statement.clone();
        statement.publisher_key = hex(key.verifying_key().as_bytes());
        assert!(
            sign(
                statement,
                &ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
            )
            .is_err()
        );
    }
}
