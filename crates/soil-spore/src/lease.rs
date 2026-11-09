//! Rendering a decided anycast claim as a Mycelium service lease.
//!
//! Mycelium decides whether a claim may route: [`crate::publisher`] verifies the
//! delegated publisher grant, and the host supplies the trust view. This module
//! only renders an already-authorized claim in the shape `myceliumd` reads from
//! `~/.mycelium/application-services.json`.
//!
//! The bounds here mirror `myceliumd/src/local_services.rs` on purpose. A lease
//! that disagrees is dropped by the daemon without an error at the Spore, so a
//! Spore that oversteps would simply never route and never say why.

use serde_json::{Value, json};

use crate::publisher::TrustedPublisher;
use crate::{Error, Lock, Manifest, Result};

/// Endpoint schemes Mycelium accepts on an application lease.
const SCHEMES: [&str; 5] = ["http", "https", "tcp", "quic", "iroh"];

/// Most endpoints one observation may carry.
const MAX_ENDPOINTS: usize = 32;

/// Build the lease document for one node.
///
/// `endpoints` are connection hints this node is reachable on. They are not
/// verified here: a loopback hint stays loopback, because claiming a public
/// address a Spore does not serve is the one thing a lease must never do.
pub fn document(
    manifest: &Manifest,
    lock: &Lock,
    publisher: &TrustedPublisher,
    node_id: &str,
    endpoints: &[String],
    now_unix_s: u64,
) -> Result<Value> {
    let Some(anycast) = &manifest.anycast else {
        return Err(Error::Invalid(
            "this Spore declares no [anycast] claim".into(),
        ));
    };
    if node_id.is_empty() || !node_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(Error::Invalid(format!(
            "{node_id:?} is not a node id"
        )));
    }
    if endpoints.is_empty() || endpoints.len() > MAX_ENDPOINTS {
        return Err(Error::Invalid(format!(
            "an anycast lease needs 1..={MAX_ENDPOINTS} endpoints"
        )));
    }
    for endpoint in endpoints {
        if !endpoint_hint(endpoint) {
            return Err(Error::Invalid(format!(
                "endpoint {endpoint:?} must be scheme://host[:port] with no credentials"
            )));
        }
    }

    let service_id = format!("service/{node_id}/application/{}", anycast.name);
    let expires_at = now_unix_s + anycast.ttl_secs;
    Ok(json!({
        "schema_version": 1,
        "observations": [{
            "schema_version": 1,
            "service_id": service_id,
            "observed_at": now_unix_s,
            "expires_at": expires_at,
            "provenance": {
                "provider": "soil-spore",
                "observer": node_id,
                "source_id": format!("{}/{}@{}", manifest.spore.id, anycast.kind, lock.digest),
            },
            "confidence": "strong",
            "value": {
                "kind": anycast.kind,
                "endpoints": endpoints,
                "protocols": [scheme_of(endpoints[0])],
                "formats": [],
                "attributes": {
                    "node_id": node_id,
                    "spore_id": manifest.spore.id.clone(),
                    "spore_version": manifest.spore.version.clone(),
                    "package_digest": lock.digest.clone(),
                    "principal": publisher.principal.clone(),
                    "grant_id": publisher.grant_id.clone(),
                    "publisher_key_id": publisher.key_id.clone(),
                    "alpns": serde_json::to_string(&anycast.alpns).unwrap_or_else(|_| "[]".into()),
                },
            },
        }],
    }))
}

/// `scheme://host[:port]`, no userinfo, no path, no query.
fn endpoint_hint(endpoint: &str) -> bool {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return false;
    };
    if !SCHEMES.contains(&scheme) || rest.is_empty() {
        return false;
    }
    !rest.contains(['@', '?', '#', '/'])
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '[' | ']'))
}

fn scheme_of(endpoint: &str) -> &str {
    endpoint.split_once("://").map(|(s, _)| s).unwrap_or("")
}
