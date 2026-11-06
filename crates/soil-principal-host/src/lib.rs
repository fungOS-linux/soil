//! Host-owned principal projection for `soil:principal`.
//!
//! The host verifies Mycelium identity or an OIDC token before constructing a
//! [`Principal`]. This crate never parses or validates a JWT itself; it maps an
//! already-verified claim set or Mycelium principal into the narrow WIT shape.

use std::sync::Arc;

use serde::Deserialize;
use wasmtime::component::{HasSelf, Linker};

wasmtime::component::bindgen!({
    path: "wit",
    world: "host",
});

use soil::principal::principal as api;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    Human,
    Agent,
    Service,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalError {
    Denied,
    NotReady,
    Stale,
    Invalid,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub actor: String,
    pub kind: PrincipalKind,
    pub display_name: Option<String>,
    pub device: Option<String>,
    pub teams: Vec<String>,
    pub roles: Vec<String>,
    pub generation: u64,
}

/// Verified OIDC claims. The host must verify issuer, audience, signature and
/// expiry before constructing this. Unknown fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct OidcClaims {
    pub iss: String,
    pub sub: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub teams: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// The stable JSON identity emitted by `mycelium access oidc verify --json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VerifiedOidc {
    pub principal: String,
    pub issuer: String,
    pub subject: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    pub expires_at: u64,
}

/// Pluggable JWT verifier. The host supplies a token; the verifier never
/// crosses the WIT boundary and never returns the token.
pub trait OidcVerifier: Send + Sync {
    fn verify(
        &self,
        token: &str,
        issuer: &str,
        audience: &str,
    ) -> Result<VerifiedOidc, PrincipalError>;
}

/// Loose coupling to Mycelium: invoke the stable `mycelium access oidc verify`
/// JSON contract. No Mycelium source dependency, daemon socket, or token in
/// argv. The token is passed only through the child process environment.
pub struct MyceliumCliVerifier {
    binary: std::path::PathBuf,
    discovery: Option<String>,
    token_env: String,
}

impl MyceliumCliVerifier {
    pub fn new(binary: impl Into<std::path::PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            discovery: None,
            token_env: "SOIL_OIDC_TOKEN".into(),
        }
    }

    pub fn with_discovery(mut self, discovery: impl Into<String>) -> Self {
        self.discovery = Some(discovery.into());
        self
    }

    pub fn with_token_env(mut self, name: impl Into<String>) -> Self {
        self.token_env = name.into();
        self
    }
}

impl OidcVerifier for MyceliumCliVerifier {
    fn verify(
        &self,
        token: &str,
        issuer: &str,
        audience: &str,
    ) -> Result<VerifiedOidc, PrincipalError> {
        let mut command = std::process::Command::new(&self.binary);
        command
            .arg("access")
            .arg("oidc")
            .arg("verify")
            .arg("--issuer")
            .arg(issuer)
            .arg("--audience")
            .arg(audience)
            .arg("--token-env")
            .arg(&self.token_env)
            .arg("--json")
            .env(&self.token_env, token);
        if let Some(discovery) = &self.discovery {
            command.arg("--discovery").arg(discovery);
        }
        let output = command.output().map_err(|_| PrincipalError::Unavailable)?;
        if !output.status.success() || output.stdout.len() > 65_536 {
            return Err(PrincipalError::Denied);
        }
        let verified: VerifiedOidc =
            serde_json::from_slice(&output.stdout).map_err(|_| PrincipalError::Invalid)?;
        if verified.expires_at <= now_unix_s() {
            return Err(PrincipalError::Stale);
        }
        let expected = format!(
            "oidc:{}#{}",
            verified.issuer.trim_end_matches('/'),
            verified.subject
        );
        if verified.principal != expected || verified.subject.trim().is_empty() {
            return Err(PrincipalError::Invalid);
        }
        Ok(verified)
    }
}

/// A Mycelium grant identity. The host has already verified the signed grant.
#[derive(Debug, Clone)]
pub struct MyceliumGrantIdentity {
    pub principal: String,
    pub roles: Vec<String>,
    pub teams: Vec<String>,
    pub device: Option<String>,
}

pub trait PrincipalProvider: Send + Sync {
    fn current(&self) -> Result<Principal, PrincipalError>;
}

impl PrincipalProvider for Principal {
    fn current(&self) -> Result<Principal, PrincipalError> {
        Ok(self.clone())
    }
}

/// Static provider for tests and local development. Production hosts should
/// replace this with a Mycelium/OIDC-backed provider.
pub struct StaticPrincipalProvider {
    principal: Principal,
}

impl StaticPrincipalProvider {
    pub fn new(principal: Principal) -> Self {
        Self { principal }
    }
}

impl PrincipalProvider for StaticPrincipalProvider {
    fn current(&self) -> Result<Principal, PrincipalError> {
        Ok(self.principal.clone())
    }
}

pub struct PrincipalHost {
    provider: Arc<dyn PrincipalProvider>,
}

#[cfg(feature = "mycelium")]
#[derive(Debug, Clone)]
pub struct MyceliumTrustProvider {
    path: std::path::PathBuf,
    issuers: std::collections::BTreeSet<String>,
    kind: PrincipalKind,
    device: Option<String>,
    team_scope_prefix: String,
}

#[cfg(feature = "mycelium")]
#[derive(Debug, serde::Deserialize)]
struct PrincipalTrustDocument {
    grant: mycelium_peer_protocol::AccessRecord,
    #[serde(default)]
    revocations: Vec<mycelium_peer_protocol::AccessRecord>,
}

#[cfg(feature = "mycelium")]
impl MyceliumTrustProvider {
    pub fn new(
        path: impl Into<std::path::PathBuf>,
        issuers: impl IntoIterator<Item = String>,
        kind: PrincipalKind,
        device: Option<String>,
    ) -> Self {
        Self {
            path: path.into(),
            issuers: issuers.into_iter().collect(),
            kind,
            device,
            team_scope_prefix: "team:".into(),
        }
    }

    pub fn with_team_scope_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.team_scope_prefix = prefix.into();
        self
    }

    fn load(&self) -> Result<Principal, PrincipalError> {
        use std::io::Read;

        if self.issuers.is_empty() {
            return Err(PrincipalError::Invalid);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&self.path)
            .and_then(|file| file.take(65_537).read_to_end(&mut bytes))
            .map_err(|_| PrincipalError::Unavailable)?;
        if bytes.len() > 65_536 {
            return Err(PrincipalError::Invalid);
        }
        let document: PrincipalTrustDocument =
            serde_json::from_slice(&bytes).map_err(|_| PrincipalError::Invalid)?;
        document
            .grant
            .verify()
            .map_err(|_| PrincipalError::Denied)?;
        if !self.issuers.contains(&document.grant.signer) || document.revocations.len() > 64 {
            return Err(PrincipalError::Denied);
        }
        let now = now_unix_s();
        for record in &document.revocations {
            record.verify().map_err(|_| PrincipalError::Denied)?;
            if !self.issuers.contains(&record.signer) {
                return Err(PrincipalError::Denied);
            }
            if let mycelium_peer_protocol::AccessStatement::Revoke { revoked_at, .. } =
                &record.statement
                && *revoked_at <= now
                && record.statement.revokes(&document.grant.statement)
            {
                return Err(PrincipalError::Denied);
            }
        }

        let mycelium_peer_protocol::AccessStatement::Grant {
            principal,
            serial,
            roles,
            scopes,
            not_before,
            not_after,
            ..
        } = &document.grant.statement
        else {
            return Err(PrincipalError::Invalid);
        };
        if principal.trim().is_empty() {
            return Err(PrincipalError::Invalid);
        }
        if now < *not_before || now >= *not_after {
            return Err(PrincipalError::Stale);
        }
        let teams = scopes
            .iter()
            .filter_map(|scope| scope.strip_prefix(&self.team_scope_prefix))
            .filter(|team| !team.is_empty())
            .map(str::to_string)
            .collect();
        Ok(Principal {
            actor: principal.clone(),
            kind: self.kind,
            display_name: None,
            device: self.device.clone(),
            teams,
            roles: roles.clone(),
            generation: *serial,
        })
    }
}

#[cfg(feature = "mycelium")]
impl PrincipalProvider for MyceliumTrustProvider {
    fn current(&self) -> Result<Principal, PrincipalError> {
        self.load()
    }
}

impl PrincipalHost {
    pub fn new<P>(provider: P) -> Self
    where
        P: PrincipalProvider + 'static,
    {
        Self {
            provider: Arc::new(provider),
        }
    }

    pub fn from_shared(provider: Arc<dyn PrincipalProvider>) -> Self {
        Self { provider }
    }
}

impl Principal {
    /// Map already-verified OIDC claims into the Soil projection.
    ///
    /// `iss` plus `sub` becomes the actor. Email and raw claim values are not
    /// exposed. The caller chooses the kind; an OIDC client-credentials token
    /// should be mapped as [`PrincipalKind::Service`].
    pub fn from_oidc(
        claims: OidcClaims,
        kind: PrincipalKind,
        device: Option<String>,
        generation: u64,
    ) -> Result<Self, PrincipalError> {
        if claims.iss.trim().is_empty() || claims.sub.trim().is_empty() {
            return Err(PrincipalError::Invalid);
        }
        Ok(Self {
            actor: format!("oidc:{}#{}", claims.iss, claims.sub),
            kind,
            display_name: claims.name.or(claims.preferred_username),
            device,
            teams: claims.teams,
            roles: claims.roles,
            generation,
        })
    }

    /// Map a verified `mycelium access oidc verify` identity. Groups become
    /// roles; team extraction remains an explicit host policy.
    pub fn from_verified_oidc(
        identity: VerifiedOidc,
        kind: PrincipalKind,
        device: Option<String>,
        generation: u64,
    ) -> Result<Self, PrincipalError> {
        if identity.expires_at <= now_unix_s() || identity.subject.trim().is_empty() {
            return Err(PrincipalError::Stale);
        }
        Ok(Self {
            actor: identity.principal,
            kind,
            display_name: None,
            device,
            teams: Vec::new(),
            roles: identity.groups,
            generation,
        })
    }

    /// Map an already-verified Mycelium grant identity.
    pub fn from_mycelium(
        identity: MyceliumGrantIdentity,
        kind: PrincipalKind,
        generation: u64,
    ) -> Result<Self, PrincipalError> {
        if identity.principal.trim().is_empty() {
            return Err(PrincipalError::Invalid);
        }
        Ok(Self {
            actor: identity.principal,
            kind,
            display_name: None,
            device: identity.device,
            teams: identity.teams,
            roles: identity.roles,
            generation,
        })
    }
}

impl api::Host for PrincipalHost {
    fn current(&mut self) -> Result<api::Identity, api::Error> {
        let principal = self.provider.current().map_err(map_error)?;
        Ok(api::Identity {
            actor: principal.actor,
            kind: map_kind(principal.kind),
            display_name: principal.display_name,
            device: principal.device,
            teams: principal.teams,
            roles: principal.roles,
            generation: principal.generation,
        })
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

fn map_kind(kind: PrincipalKind) -> api::Kind {
    match kind {
        PrincipalKind::Human => api::Kind::Human,
        PrincipalKind::Agent => api::Kind::Agent,
        PrincipalKind::Service => api::Kind::Service,
    }
}

fn map_error(error: PrincipalError) -> api::Error {
    match error {
        PrincipalError::Denied => api::Error::Denied,
        PrincipalError::NotReady => api::Error::NotReady,
        PrincipalError::Stale => api::Error::Stale,
        PrincipalError::Invalid => api::Error::Invalid,
        PrincipalError::Unavailable => api::Error::Unavailable,
    }
}

fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod oidc_cli_tests {
    use super::*;

    #[cfg(unix)]
    fn fake_verifier(dir: &std::path::Path, payload: &str) -> MyceliumCliVerifier {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("mycelium");
        std::fs::write(&path, format!("#!/bin/sh\ncat <<'JSON'\n{payload}\nJSON\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        MyceliumCliVerifier::new(path)
    }

    #[cfg(unix)]
    #[test]
    fn mycelium_cli_verifier_parses_the_stable_json_contract() {
        let dir = tempfile::tempdir().unwrap();
        let verifier = fake_verifier(
            dir.path(),
            r#"{"principal":"oidc:https://identity.example#user-42","issuer":"https://identity.example","subject":"user-42","email":"a@example.test","groups":["operators"],"expires_at":9999999999}"#,
        );
        let verified = verifier
            .verify("token", "https://identity.example", "soil")
            .unwrap();
        assert_eq!(verified.principal, "oidc:https://identity.example#user-42");
        assert_eq!(verified.groups, vec!["operators"]);
        let principal = Principal::from_verified_oidc(
            verified,
            PrincipalKind::Human,
            Some("device-1".into()),
            4,
        )
        .unwrap();
        assert_eq!(principal.roles, vec!["operators"]);
        assert_eq!(principal.generation, 4);
    }

    #[cfg(unix)]
    #[test]
    fn mycelium_cli_verifier_denies_failed_commands() {
        let dir = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("mycelium");
        std::fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        let verifier = MyceliumCliVerifier::new(path);
        assert_eq!(
            verifier.verify("token", "https://identity.example", "soil"),
            Err(PrincipalError::Denied)
        );
    }
}

#[cfg(all(test, feature = "mycelium"))]
mod mycelium_tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use mycelium_peer_protocol::{AccessRecord, AccessStatement};

    fn signed_document(dir: &std::path::Path, revoked: bool) -> (std::path::PathBuf, String) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let issuer = mycelium_peer_protocol::encode_hex(key.verifying_key().as_bytes());
        let now = now_unix_s();
        let statement = AccessStatement::Grant {
            grant_id: "principal-test".into(),
            principal: "oidc:https://identity.example#subject-42".into(),
            serial: 9,
            roles: vec!["operator".into()],
            scopes: vec!["team:fpl".into()],
            unix_users: Vec::new(),
            ssh_public_keys: Vec::new(),
            oidc_audiences: vec!["soil".into()],
            oidc_ssh_key_exchange: false,
            not_before: now.saturating_sub(60),
            not_after: now + 3600,
        };
        let grant = AccessRecord::sign(&key, statement).unwrap();
        let revocations = if revoked {
            vec![
                AccessRecord::sign(
                    &key,
                    AccessStatement::Revoke {
                        revocation_id: "revoke-test".into(),
                        grant_id: Some("principal-test".into()),
                        principal: None,
                        serial: None,
                        revoked_at: now,
                        reason: "test".into(),
                    },
                )
                .unwrap(),
            ]
        } else {
            Vec::new()
        };
        let path = dir.join("trust.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "grant": grant,
                "revocations": revocations,
                "permit": {}
            }))
            .unwrap(),
        )
        .unwrap();
        (path, issuer)
    }

    #[test]
    fn mycelium_trust_document_maps_to_a_principal() {
        let dir = tempfile::tempdir().unwrap();
        let (path, issuer) = signed_document(dir.path(), false);
        let provider = MyceliumTrustProvider::new(
            path,
            [issuer],
            PrincipalKind::Human,
            Some("device-1".into()),
        );
        let principal = provider.current().unwrap();
        assert_eq!(principal.actor, "oidc:https://identity.example#subject-42");
        assert_eq!(principal.kind, PrincipalKind::Human);
        assert_eq!(principal.teams, vec!["fpl"]);
        assert_eq!(principal.roles, vec!["operator"]);
        assert_eq!(principal.generation, 9);
    }

    #[test]
    fn revoked_mycelium_grant_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let (path, issuer) = signed_document(dir.path(), true);
        let provider = MyceliumTrustProvider::new(path, [issuer], PrincipalKind::Human, None);
        assert_eq!(provider.current(), Err(PrincipalError::Denied));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oidc_maps_to_a_stable_principal_without_email() {
        let claims: OidcClaims = serde_json::from_value(serde_json::json!({
            "iss": "https://identity.example",
            "sub": "subject-42",
            "name": "Avery",
            "email": "avery@example.invalid",
            "teams": ["fpl"],
            "roles": ["operator"]
        }))
        .unwrap();
        let principal =
            Principal::from_oidc(claims, PrincipalKind::Human, Some("device-1".into()), 7).unwrap();
        assert_eq!(principal.actor, "oidc:https://identity.example#subject-42");
        assert_eq!(principal.display_name.as_deref(), Some("Avery"));
        assert_eq!(principal.teams, vec!["fpl"]);
        assert_eq!(principal.roles, vec!["operator"]);
        assert_eq!(principal.device.as_deref(), Some("device-1"));
        assert_eq!(principal.generation, 7);
    }

    #[test]
    fn mycelium_grant_maps_to_principal() {
        let principal = Principal::from_mycelium(
            MyceliumGrantIdentity {
                principal: "oidc:https://identity.example#subject-42".into(),
                roles: vec!["operator".into()],
                teams: vec!["fpl".into()],
                device: Some("device-1".into()),
            },
            PrincipalKind::Human,
            3,
        )
        .unwrap();
        assert_eq!(principal.actor, "oidc:https://identity.example#subject-42");
        assert_eq!(principal.kind, PrincipalKind::Human);
    }

    #[test]
    fn empty_oidc_subject_is_invalid() {
        let claims = OidcClaims {
            iss: "https://identity.example".into(),
            sub: " ".into(),
            name: None,
            preferred_username: None,
            teams: Vec::new(),
            roles: Vec::new(),
        };
        assert_eq!(
            Principal::from_oidc(claims, PrincipalKind::Human, None, 1),
            Err(PrincipalError::Invalid)
        );
    }
}
