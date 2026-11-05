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
