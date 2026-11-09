//! Who this node is, read from the identity it was enrolled with.
//!
//! Nobody types a node id on an ephemeral machine. `terraform apply` mints an
//! invite, cloud-init carries it, and the first boot redeems it with
//! `mycelium setup --claim`. What that leaves on disk — `pki/node.pem` and the
//! environment beside it — is therefore the only identity an unattended Spore
//! can honestly announce under.
//!
//! Reading it is what makes a rebooted node reappear as *itself*, while a
//! re-enrolled node becomes genuinely new: no long-lived state is bound to a
//! node id, so losing the certificate is an ordinary event rather than an
//! incident.
//!
//! The id is derived, not recorded, so it cannot drift from the certificate it
//! describes. Two consequences are worth stating because they are the whole
//! point: re-wrapping the PEM file changes nothing, and enrolling a second time
//! changes everything.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::{Error, Result, hex};

/// Where Mycelium keeps the node certificate, relative to its home.
pub const CERTIFICATE: &str = "pki/node.pem";

/// Where Mycelium keeps the node environment, relative to its home.
pub const ENVIRONMENT: &str = "node.env";

/// Leading hex digits of the certificate digest that make up a node id.
///
/// 64 bits, which is far wider than a fleet: the value only has to separate
/// the replicas alive at one time, and a short id keeps `service/<id>/…`
/// readable in a gateway log.
const NODE_ID_CHARS: usize = 16;

/// The identity a lease is published under.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Identity {
    /// Hex digest of the node certificate. Stable across restarts, new on
    /// re-enrollment.
    pub node_id: String,
    /// The `.myc` name the certificate carries, when it carries one. Not part
    /// of the lease identity — two replicas may share a name — but it is what
    /// turns `service/3f9a…` into a host an operator can find.
    pub name: Option<String>,
    /// The site the node was enrolled into.
    pub site: Option<String>,
}

impl Identity {
    /// Read the identity Mycelium installed under `home`.
    ///
    /// Only the certificate is required. A node environment without a name is
    /// ordinary — bundles issued before fleet names existed carry none — and
    /// must not stop a node from announcing.
    pub fn read(home: &Path) -> Result<Self> {
        let certificate = home.join(CERTIFICATE);
        let pem = std::fs::read_to_string(&certificate).map_err(|error| {
            Error::Invalid(format!(
                "{}: {error}. This node has no enrolled identity, so it has no id to \
                 announce under. Enroll it first (`mycelium setup --claim …`), or point \
                 --home at the Mycelium home it was enrolled under",
                certificate.display()
            ))
        })?;
        let environment = std::fs::read_to_string(home.join(ENVIRONMENT)).unwrap_or_default();
        Self::derived(&pem, &environment)
    }

    /// The identity a certificate denotes.
    ///
    /// Split from [`Identity::read`] so the derivation can be exercised without
    /// a filesystem: the properties that matter are about how the id is
    /// computed, not about where the bytes came from.
    pub fn derived(certificate_pem: &str, environment: &str) -> Result<Self> {
        let body: Vec<u8> = certificate_pem
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        if body.is_empty() {
            // An empty certificate would hash to one id shared by every
            // node with an empty certificate, which is worse than refusing.
            return Err(Error::Invalid(format!(
                "{} is empty, so this node has no identity to derive",
                CERTIFICATE
            )));
        }
        let node_id = hex(&Sha256::digest(&body))[..NODE_ID_CHARS].to_string();
        Ok(Self {
            node_id,
            name: setting(environment, "MYCELIUM_IROH_SERVER_NAME"),
            site: setting(environment, "MYCELIUM_SITE"),
        })
    }
}

/// One `KEY=value` line from the node environment.
///
/// The environment wraps long values across lines (`MYCELIUM_PEERS` is broken
/// deliberately), so this reads line by line and takes the whole rest of the
/// line as the value — no continuation handling, because none of the single
/// valued keys need any.
fn setting(environment: &str, key: &str) -> Option<String> {
    environment
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    const PEM: &str =
        "-----BEGIN CERTIFICATE-----\nMIIBszCCAVmgAwIBAgIUQ==\n-----END CERTIFICATE-----\n";

    #[test]
    fn a_restart_finds_the_same_node() {
        let first = Identity::derived(PEM, "").expect("a certificate yields an id");
        let again = Identity::derived(PEM, "").expect("a certificate yields an id");
        assert_eq!(first.node_id, again.node_id);
        assert_eq!(first.node_id.len(), NODE_ID_CHARS);
        assert!(first.node_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn re_wrapping_the_certificate_does_not_mint_a_new_node() {
        let rewrapped = PEM.replace('\n', "\r\n\n");
        assert_eq!(
            Identity::derived(PEM, "").expect("id").node_id,
            Identity::derived(&rewrapped, "").expect("id").node_id
        );
    }

    #[test]
    fn re_enrolling_mints_a_new_node() {
        let other = PEM.replace("Q==", "R==");
        assert_ne!(
            Identity::derived(PEM, "").expect("id").node_id,
            Identity::derived(&other, "").expect("id").node_id
        );
    }

    #[test]
    fn an_empty_certificate_is_refused_rather_than_shared() {
        assert!(Identity::derived(" \n\t", "").is_err());
    }

    #[test]
    fn a_name_comes_from_the_environment_beside_the_certificate() {
        let environment = "MYCELIUM_SITE=wagar-house\nMYCELIUM_PEERS=a.myc@1.2.3.4:7443,b.myc@5.6.7.8:7443\nMYCELIUM_IROH_SERVER_NAME=mac-studio.wagar-house.myc\n";
        let identity = Identity::derived(PEM, environment).expect("id");
        assert_eq!(identity.name.as_deref(), Some("mac-studio.wagar-house.myc"));
        assert_eq!(identity.site.as_deref(), Some("wagar-house"));
    }

    #[test]
    fn a_node_without_a_name_still_has_an_identity() {
        let identity = Identity::derived(PEM, "MYCELIUM_SITE=beachhead\n").expect("id");
        assert_eq!(identity.name, None);
        assert_eq!(identity.site.as_deref(), Some("beachhead"));
    }

    #[test]
    fn an_unenrolled_node_is_told_what_to_run() {
        let empty = std::env::temp_dir().join(format!(
            "soil-spore-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&empty).expect("scratch dir");
        let refused = Identity::read(&empty).expect_err("no certificate, no identity");
        let message = refused.to_string();
        assert!(message.contains("mycelium setup"), "{message}");
        std::fs::remove_dir_all(empty).expect("cleanup");
    }
}
