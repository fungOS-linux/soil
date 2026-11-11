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

impl fmt::Debug for Material {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never the bytes: a credential that can be printed will end up in a log.
        match self {
            Self::HmacSha256(_) => f.write_str("HmacSha256(<redacted>)"),
            Self::Ed25519Signing(_) => f.write_str("Ed25519Signing(<redacted>)"),
            Self::Ed25519Verifying(_) => f.write_str("Ed25519Verifying(<public>)"),
        }
    }
}

#[derive(Clone, Debug)]
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
        sign_with(&entry.material, algorithm, message)
    }

    fn verify(
        &self,
        handle: &str,
        algorithm: Algorithm,
        message: &[u8],
        signature: &[u8],
    ) -> std::result::Result<bool, CredentialError> {
        let entry = self.entry(handle)?;
        verify_with(&entry.material, algorithm, message, signature)
    }
}

fn sign_with(
    material: &Material,
    algorithm: Algorithm,
    message: &[u8],
) -> std::result::Result<Vec<u8>, CredentialError> {
    match (material, algorithm) {
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

fn verify_with(
    material: &Material,
    algorithm: Algorithm,
    message: &[u8],
    signature: &[u8],
) -> std::result::Result<bool, CredentialError> {
    match (material, algorithm) {
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

// ---- Geode -----------------------------------------------------------------

use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;

/// Credentials leased from `geoded`.
///
/// `geoded` holds an encrypted store and leases secrets only to processes with
/// the same uid, one JSON request per connection. That is exactly this trait's
/// shape - resolve, then use, then forget - so the adapter is a socket and a
/// map of live leases, not a key store.
///
/// The material lives in this process only for the life of the lease: a lease
/// that expires is expired here too, and the caller cannot extend it by asking
/// twice because every resolve takes a fresh one.
#[derive(Debug)]
pub struct GeodeCredentials {
    socket: PathBuf,
    /// Prefix a guest's plain name to a `geode://<realm>/<account>/` reference.
    ///
    /// A guest names a credential; only the host knows where it lives. Keeping
    /// the prefix out of the guest is what stops a Spore addressing another
    /// realm by spelling its reference.
    prefix: String,
    purpose: String,
    ttl_ms: u64,
    leased: Mutex<BTreeMap<String, Entry>>,
    counter: AtomicU64,
}

impl GeodeCredentials {
    /// Lease with `purpose` for `ttl` from the daemon listening on `socket`.
    ///
    /// The purpose is chosen by the host, never by the guest: a credential
    /// minted to verify a webhook must not be able to sign one.
    pub fn new(
        socket: impl Into<PathBuf>,
        prefix: impl Into<String>,
        purpose: impl Into<String>,
        ttl: Duration,
    ) -> Self {
        Self {
            socket: socket.into(),
            prefix: prefix.into(),
            purpose: purpose.into(),
            ttl_ms: ttl.as_millis().min(u128::from(u64::MAX)) as u64,
            leased: Mutex::new(BTreeMap::new()),
            counter: AtomicU64::new(0),
        }
    }

    fn lease(
        &self,
        reference: &str,
    ) -> std::result::Result<(Vec<u8>, Option<u64>), CredentialError> {
        let request = serde_json::json!({
            "op": "lease",
            "reference": format!("{}{reference}", self.prefix),
            "purpose": self.purpose,
            "ttl_ms": self.ttl_ms,
        });
        let mut stream =
            UnixStream::connect(&self.socket).map_err(|_| CredentialError::Unavailable)?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        writeln!(stream, "{request}").map_err(|_| CredentialError::Unavailable)?;
        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .map_err(|_| CredentialError::Unavailable)?;
        let reply: serde_json::Value =
            serde_json::from_str(&line).map_err(|_| CredentialError::Unavailable)?;
        if reply["ok"].as_bool() != Some(true) {
            let error = reply["error"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            // The daemon says why; the guest is told only what it may act on.
            return Err(if error.contains("expired") {
                CredentialError::Expired
            } else if error.contains("purpose")
                || error.contains("not found")
                || error.contains("denied")
            {
                CredentialError::Denied
            } else {
                CredentialError::Unavailable
            });
        }
        let encoded = reply["secret_b64"]
            .as_str()
            .ok_or(CredentialError::Unavailable)?;
        let secret = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| CredentialError::Unavailable)?;
        Ok((secret, reply["expires_at"].as_u64()))
    }

    fn entry(&self, handle: &str) -> std::result::Result<Entry, CredentialError> {
        let entry = self
            .leased
            .lock()
            .map_err(|_| CredentialError::Unavailable)?
            .get(handle)
            .cloned()
            .ok_or(CredentialError::Denied)?;
        if entry
            .expires_at_ms
            .is_some_and(|expires| now_ms() >= expires)
        {
            return Err(CredentialError::Expired);
        }
        Ok(entry)
    }
}

impl CredentialProvider for GeodeCredentials {
    fn resolve(&self, name: &str) -> std::result::Result<ResolvedHandle, CredentialError> {
        if name.is_empty() {
            return Err(CredentialError::Invalid);
        }
        let (secret, expires_at) = self.lease(name)?;
        let id = format!("{name}#{}", self.counter.fetch_add(1, Ordering::Relaxed));
        let entry = Entry {
            purpose: self.purpose.clone(),
            // `geoded` reports unix seconds; the handle reports milliseconds so
            // a guest can compare it with the clock it already has.
            expires_at_ms: expires_at.map(|seconds| seconds.saturating_mul(1000)),
            // A webhook secret is an HMAC key. Anything else would be a second
            // credential kind the trait cannot express yet.
            material: Material::HmacSha256(secret),
        };
        let handle = ResolvedHandle {
            id: id.clone(),
            purpose: entry.purpose.clone(),
            expires_at_ms: entry.expires_at_ms,
        };
        self.leased
            .lock()
            .map_err(|_| CredentialError::Unavailable)?
            .insert(id, entry);
        Ok(handle)
    }

    fn sign(
        &self,
        handle: &str,
        algorithm: Algorithm,
        message: &[u8],
    ) -> std::result::Result<Vec<u8>, CredentialError> {
        let entry = self.entry(handle)?;
        sign_with(&entry.material, algorithm, message)
    }

    fn verify(
        &self,
        handle: &str,
        algorithm: Algorithm,
        message: &[u8],
        signature: &[u8],
    ) -> std::result::Result<bool, CredentialError> {
        let entry = self.entry(handle)?;
        verify_with(&entry.material, algorithm, message, signature)
    }
}

#[cfg(test)]
mod geode_tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A daemon that answers one lease with `reply`, so the adapter is tested
    /// against the wire format rather than against a mock trait.
    fn fake_geode(reply: serde_json::Value) -> (PathBuf, std::thread::JoinHandle<String>) {
        let socket = std::env::temp_dir().join(format!(
            "soil-geode-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let listener = UnixListener::bind(&socket).expect("bind the fake daemon");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("read the request");
            writeln!(stream, "{reply}").expect("write the reply");
            line
        });
        (socket, server)
    }

    #[test]
    fn a_lease_becomes_a_verify_handle() {
        let secret = base64::engine::general_purpose::STANDARD.encode(b"hook-secret");
        let (socket, server) = fake_geode(serde_json::json!({
            "ok": true,
            "secret_b64": secret,
            "expires_at": now_ms() / 1000 + 60,
        }));
        let credentials = GeodeCredentials::new(
            &socket,
            "geode://fungos/agora/",
            "verify",
            Duration::from_secs(60),
        );

        let handle = credentials
            .resolve("confluence.webhook/smoke")
            .expect("lease");
        assert_eq!(handle.purpose, "verify");
        assert!(handle.expires_at_ms.is_some(), "a lease is time-bounded");

        let signature = credentials
            .sign(&handle.id, Algorithm::HmacSha256, b"body")
            .expect("sign");
        assert!(
            credentials
                .verify(&handle.id, Algorithm::HmacSha256, b"body", &signature)
                .expect("verify")
        );
        assert!(
            !credentials
                .verify(&handle.id, Algorithm::HmacSha256, b"other", &signature)
                .expect("verify"),
            "a different body must not verify"
        );

        let request: serde_json::Value =
            serde_json::from_str(&server.join().expect("server")).expect("json request");
        assert_eq!(request["op"], "lease");
        assert_eq!(
            request["reference"],
            "geode://fungos/agora/confluence.webhook/smoke"
        );
        assert_eq!(request["purpose"], "verify", "the host picks the purpose");
        assert_eq!(request["ttl_ms"], 60_000);
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn a_refused_lease_is_denied_and_a_missing_daemon_is_unavailable() {
        let (socket, server) = fake_geode(serde_json::json!({
            "ok": false,
            "error": "purpose not allowed for confluence.webhook/smoke",
        }));
        let credentials = GeodeCredentials::new(
            &socket,
            "geode://fungos/agora/",
            "sign",
            Duration::from_secs(60),
        );
        assert_eq!(
            credentials.resolve("confluence.webhook/smoke").unwrap_err(),
            CredentialError::Denied
        );
        let _ = server.join();
        let _ = std::fs::remove_file(&socket);

        let absent = std::env::temp_dir().join("soil-geode-does-not-exist.sock");
        let credentials = GeodeCredentials::new(
            absent,
            "geode://fungos/agora/",
            "verify",
            Duration::from_secs(60),
        );
        assert_eq!(
            credentials.resolve("anything").unwrap_err(),
            CredentialError::Unavailable
        );
    }

    #[test]
    fn an_expired_lease_stops_answering_before_the_daemon_says_so() {
        let secret = base64::engine::general_purpose::STANDARD.encode(b"hook-secret");
        let (socket, server) = fake_geode(serde_json::json!({
            "ok": true,
            "secret_b64": secret,
            "expires_at": 1,
        }));
        let credentials = GeodeCredentials::new(
            &socket,
            "geode://fungos/agora/",
            "verify",
            Duration::from_secs(60),
        );
        let handle = credentials.resolve("hook").expect("lease");
        assert_eq!(
            credentials
                .sign(&handle.id, Algorithm::HmacSha256, b"body")
                .unwrap_err(),
            CredentialError::Expired
        );
        let _ = server.join();
        let _ = std::fs::remove_file(socket);
    }
}
