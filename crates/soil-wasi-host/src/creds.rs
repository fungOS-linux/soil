//! `soil:creds`: host-held signing and verification.
//!
//! No secret bytes cross the boundary. A guest resolves a granted name to an
//! opaque handle and asks the host to sign or verify. The provider re-checks
//! the grant and expiry on every call, so a forged or stale handle fails.

use std::{collections::BTreeMap, fmt};

use anyhow::{Context as _, Result, bail};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use wasmtime::component::{HasSelf, Linker};

use crate::{context::now_ms, host::WasiHost};

wasmtime::component::bindgen!({
    path: "../../wit/soil-creds.wit",
    world: "host",
});

use soil::creds::creds as api;

/// Signature algorithms a host provider may support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    HmacSha256,
    Ed25519,
}

/// Typed credential failure. Mirrors `soil:creds/error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialError {
    Denied,
    Invalid,
    Expired,
    Unavailable,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied => f.write_str("credential denied"),
            Self::Invalid => f.write_str("invalid credential request"),
            Self::Expired => f.write_str("credential expired"),
            Self::Unavailable => f.write_str("credential backend unavailable"),
        }
    }
}

impl std::error::Error for CredentialError {}

/// The non-secret projection of a resolved credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedHandle {
    pub id: String,
    pub purpose: String,
    pub expires_at_ms: Option<u64>,
}

/// Host-side credential backend.
///
/// The contract is intentionally narrow: resolve by name, sign, verify. It has
/// no read, list, or export operation. A Geode adapter implements this trait by
/// talking to Mycelium; [`StaticCredentials`] is the in-process test and local
/// operator implementation.
pub trait CredentialProvider: Send + Sync {
    fn resolve(&self, name: &str) -> std::result::Result<ResolvedHandle, CredentialError>;

    fn sign(
        &self,
        handle: &str,
        algorithm: Algorithm,
        message: &[u8],
    ) -> std::result::Result<Vec<u8>, CredentialError>;

    fn verify(
        &self,
        handle: &str,
        algorithm: Algorithm,
        message: &[u8],
        signature: &[u8],
    ) -> std::result::Result<bool, CredentialError>;
}

#[derive(Clone)]
enum Material {
    HmacSha256(Vec<u8>),
    Ed25519Signing(SigningKey),
    Ed25519Verifying(VerifyingKey),
}

#[derive(Clone)]
struct Entry {
    purpose: String,
    expires_at_ms: Option<u64>,
    material: Material,
}

/// In-process credential provider.
///
/// Only names inserted here are resolvable. This is the local proof backend;
/// production wiring resolves against Geode and keeps material out of the host
/// process entirely.
#[derive(Clone, Default)]
pub struct StaticCredentials {
    entries: BTreeMap<String, Entry>,
}

impl StaticCredentials {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an HMAC-SHA256 secret. HMAC can both sign and verify.
    pub fn insert_hmac(
        &mut self,
        name: impl Into<String>,
        secret: impl Into<Vec<u8>>,
        purpose: impl Into<String>,
    ) -> Result<()> {
        self.insert(
            name.into(),
            Entry {
                purpose: purpose.into(),
                expires_at_ms: None,
                material: Material::HmacSha256(secret.into()),
            },
        )
    }

    /// Insert an Ed25519 signing key. Signing keys can also verify.
    pub fn insert_ed25519_signing(
        &mut self,
        name: impl Into<String>,
        key: [u8; 32],
        purpose: impl Into<String>,
    ) -> Result<()> {
        self.insert(
            name.into(),
            Entry {
                purpose: purpose.into(),
                expires_at_ms: None,
                material: Material::Ed25519Signing(SigningKey::from_bytes(&key)),
            },
        )
    }

    /// Insert an Ed25519 public key. Verify-only credentials refuse `sign`.
    pub fn insert_ed25519_verifying(
        &mut self,
        name: impl Into<String>,
        key: [u8; 32],
        purpose: impl Into<String>,
    ) -> Result<()> {
        let key = VerifyingKey::from_bytes(&key).context("parse ed25519 public key")?;
        self.insert(
            name.into(),
            Entry {
                purpose: purpose.into(),
                expires_at_ms: None,
                material: Material::Ed25519Verifying(key),
            },
        )
    }

    /// Bound an existing credential to a host wall-clock expiry.
    pub fn set_expiry(&mut self, name: &str, expires_at_ms: u64) -> Result<()> {
        let entry = self
            .entries
            .get_mut(name)
            .with_context(|| format!("unknown credential {name}"))?;
        entry.expires_at_ms = Some(expires_at_ms);
        Ok(())
    }

    fn insert(&mut self, name: String, entry: Entry) -> Result<()> {
        if name.is_empty() {
            bail!("credential name must not be empty");
        }
        if self.entries.contains_key(&name) {
            bail!("credential {name} is already defined");
        }
        self.entries.insert(name, entry);
        Ok(())
    }

    fn entry(&self, handle: &str) -> std::result::Result<&Entry, CredentialError> {
        let entry = self.entries.get(handle).ok_or(CredentialError::Denied)?;
        if entry
            .expires_at_ms
            .is_some_and(|expires| now_ms() >= expires)
        {
            return Err(CredentialError::Expired);
        }
        Ok(entry)
    }
}

impl CredentialProvider for StaticCredentials {
    fn resolve(&self, name: &str) -> std::result::Result<ResolvedHandle, CredentialError> {
        let entry = self.entry(name)?;
        Ok(ResolvedHandle {
            id: name.to_string(),
            purpose: entry.purpose.clone(),
            expires_at_ms: entry.expires_at_ms,
        })
    }

    fn sign(
        &self,
        handle: &str,
        algorithm: Algorithm,
        message: &[u8],
    ) -> std::result::Result<Vec<u8>, CredentialError> {
        let entry = self.entry(handle)?;
        match (&entry.material, algorithm) {
            (Material::HmacSha256(secret), Algorithm::HmacSha256) => {
                let mut mac =
                    Hmac::<Sha256>::new_from_slice(secret).map_err(|_| CredentialError::Invalid)?;
                mac.update(message);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            (Material::Ed25519Signing(key), Algorithm::Ed25519) => {
                Ok(key.sign(message).to_bytes().to_vec())
            }
            (Material::Ed25519Verifying(_), Algorithm::Ed25519) => Err(CredentialError::Denied),
            _ => Err(CredentialError::Invalid),
        }
    }

    fn verify(
        &self,
        handle: &str,
        algorithm: Algorithm,
        message: &[u8],
        signature: &[u8],
    ) -> std::result::Result<bool, CredentialError> {
        let entry = self.entry(handle)?;
        match (&entry.material, algorithm) {
            (Material::HmacSha256(secret), Algorithm::HmacSha256) => {
                let mut mac =
                    Hmac::<Sha256>::new_from_slice(secret).map_err(|_| CredentialError::Invalid)?;
                mac.update(message);
                Ok(mac.verify_slice(signature).is_ok())
            }
            (Material::Ed25519Signing(key), Algorithm::Ed25519) => {
                verify_ed25519(&key.verifying_key(), message, signature)
            }
            (Material::Ed25519Verifying(key), Algorithm::Ed25519) => {
                verify_ed25519(key, message, signature)
            }
            _ => Err(CredentialError::Invalid),
        }
    }
}

fn verify_ed25519(
    key: &VerifyingKey,
    message: &[u8],
    signature: &[u8],
) -> std::result::Result<bool, CredentialError> {
    let signature = Signature::from_slice(signature).map_err(|_| CredentialError::Invalid)?;
    Ok(key.verify(message, &signature).is_ok())
}

impl api::Host for WasiHost {
    fn resolve(&mut self, name: String) -> std::result::Result<api::Handle, api::Error> {
        let provider = self.credential_provider().ok_or(api::Error::Unavailable)?;
        let handle = provider.resolve(&name).map_err(map_error)?;
        Ok(api::Handle {
            id: handle.id,
            purpose: handle.purpose,
            expires_at_ms: handle.expires_at_ms,
        })
    }

    fn sign(
        &mut self,
        handle: api::Handle,
        algorithm: api::Algorithm,
        message: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, api::Error> {
        let provider = self.credential_provider().ok_or(api::Error::Unavailable)?;
        provider
            .sign(&handle.id, map_algorithm(algorithm), &message)
            .map_err(map_error)
    }

    fn verify(
        &mut self,
        handle: api::Handle,
        algorithm: api::Algorithm,
        message: Vec<u8>,
        signature: Vec<u8>,
    ) -> std::result::Result<bool, api::Error> {
        let provider = self.credential_provider().ok_or(api::Error::Unavailable)?;
        provider
            .verify(&handle.id, map_algorithm(algorithm), &message, &signature)
            .map_err(map_error)
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

fn map_algorithm(algorithm: api::Algorithm) -> Algorithm {
    match algorithm {
        api::Algorithm::HmacSha256 => Algorithm::HmacSha256,
        api::Algorithm::Ed25519 => Algorithm::Ed25519,
    }
}

fn map_error(error: CredentialError) -> api::Error {
    match error {
        CredentialError::Denied => api::Error::Denied,
        CredentialError::Invalid => api::Error::Invalid,
        CredentialError::Expired => api::Error::Expired,
        CredentialError::Unavailable => api::Error::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{WasiHostBuilder, WasiHostPolicy};

    fn hmac() -> StaticCredentials {
        let mut credentials = StaticCredentials::new();
        credentials
            .insert_hmac(
                "webhook",
                b"a webhook secret".to_vec(),
                "verify provider webhooks",
            )
            .unwrap();
        credentials
    }

    #[test]
    fn hmac_sign_and_verify_round_trip() {
        let credentials = hmac();
        let handle = credentials.resolve("webhook").unwrap();
        let signature = credentials
            .sign(&handle.id, Algorithm::HmacSha256, b"body")
            .unwrap();
        assert!(
            credentials
                .verify(&handle.id, Algorithm::HmacSha256, b"body", &signature)
                .unwrap()
        );
        assert!(
            !credentials
                .verify(&handle.id, Algorithm::HmacSha256, b"other", &signature)
                .unwrap()
        );
    }

    #[test]
    fn unknown_and_expired_handles_fail_closed() {
        let mut credentials = hmac();
        assert_eq!(
            credentials.resolve("missing").unwrap_err(),
            CredentialError::Denied
        );
        credentials
            .set_expiry("webhook", crate::context::now_ms().saturating_sub(1))
            .unwrap();
        assert_eq!(
            credentials.resolve("webhook").unwrap_err(),
            CredentialError::Expired
        );
    }

    #[test]
    fn ed25519_signs_and_verifies_but_does_not_export_keys() {
        let mut credentials = StaticCredentials::new();
        credentials
            .insert_ed25519_signing("signer", [7u8; 32], "sign outbound events")
            .unwrap();
        let signing = credentials.resolve("signer").unwrap();
        let signature = credentials
            .sign(&signing.id, Algorithm::Ed25519, b"message")
            .unwrap();
        assert!(
            credentials
                .verify(&signing.id, Algorithm::Ed25519, b"message", &signature)
                .unwrap()
        );

        let public = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes();
        credentials
            .insert_ed25519_verifying("verifier", public, "verify outbound events")
            .unwrap();
        let verifying = credentials.resolve("verifier").unwrap();
        assert!(
            credentials
                .verify(&verifying.id, Algorithm::Ed25519, b"message", &signature)
                .unwrap()
        );
        assert_eq!(
            credentials
                .sign(&verifying.id, Algorithm::Ed25519, b"message")
                .unwrap_err(),
            CredentialError::Denied
        );
    }

    #[test]
    fn wit_surface_resolves_and_uses_a_handle() {
        let builder =
            WasiHostBuilder::new(WasiHostPolicy::default()).credential_provider(Arc::new(hmac()));
        let mut host = builder.build().unwrap();

        let handle = api::Host::resolve(&mut host, "webhook".to_string()).unwrap();
        assert_eq!(handle.id, "webhook");
        assert_eq!(handle.purpose, "verify provider webhooks");
        assert_eq!(handle.expires_at_ms, None);

        let signature = api::Host::sign(
            &mut host,
            handle.clone(),
            api::Algorithm::HmacSha256,
            b"body".to_vec(),
        )
        .unwrap();
        assert!(
            api::Host::verify(
                &mut host,
                handle,
                api::Algorithm::HmacSha256,
                b"body".to_vec(),
                signature,
            )
            .unwrap()
        );

        assert_eq!(
            api::Host::resolve(&mut host, "missing".to_string()).unwrap_err(),
            api::Error::Denied
        );
    }

    #[test]
    fn no_provider_is_unavailable_not_denied_by_default() {
        let mut host = WasiHostBuilder::new(WasiHostPolicy::default())
            .build()
            .unwrap();
        assert_eq!(
            api::Host::resolve(&mut host, "webhook".to_string()).unwrap_err(),
            api::Error::Unavailable
        );
    }
}
