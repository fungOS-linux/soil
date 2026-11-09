//! SNI to backend grants.
//!
//! The edge matches exact hostnames. There is no wildcard, suffix, or default
//! route: an SNI the operator did not grant is closed before a backend is
//! contacted.

use std::{collections::BTreeMap, fmt, net::SocketAddr};

use anyhow::{Context as _, Result, bail};
use iroh::EndpointId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Plain TCP origin, usually a Soil ingress gateway on this host.
    Tcp(SocketAddr),
    /// Iroh endpoint with a direct address. Relay-only endpoints are deferred.
    Iroh {
        endpoint: EndpointId,
        addr: SocketAddr,
    },
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "tcp://{addr}"),
            Self::Iroh { endpoint, addr } => write!(f, "iroh://{endpoint}@{addr}"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Routes {
    entries: BTreeMap<String, Backend>,
}

impl Routes {
    /// Parse `SNI=BACKEND` specs.
    ///
    /// Backends are `tcp://HOST:PORT` or `iroh://ENDPOINT_ID@HOST:PORT`. The
    /// Iroh form carries a direct address so the edge does not depend on a
    /// relay or DNS to reach the first hop.
    pub fn parse(specs: &[String]) -> Result<Self> {
        let mut routes = Self::default();
        for spec in specs {
            let (sni, backend) = spec
                .split_once('=')
                .with_context(|| format!("route {spec:?} must be SNI=BACKEND"))?;
            routes.insert(sni, parse_backend(backend)?)?;
        }
        Ok(routes)
    }

    pub fn insert(&mut self, sni: &str, backend: Backend) -> Result<()> {
        let sni = normalize_sni(sni)?;
        if self.entries.contains_key(&sni) {
            bail!("route {sni} is already defined");
        }
        self.entries.insert(sni, backend);
        Ok(())
    }

    pub fn resolve(&self, sni: &str) -> Option<&Backend> {
        let sni = sni.trim().trim_end_matches('.').to_ascii_lowercase();
        self.entries.get(&sni)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Backend)> {
        self.entries
            .iter()
            .map(|(sni, backend)| (sni.as_str(), backend))
    }
}

fn parse_backend(spec: &str) -> Result<Backend> {
    if let Some(addr) = spec.strip_prefix("tcp://") {
        return Ok(Backend::Tcp(
            addr.parse()
                .with_context(|| format!("parse TCP backend address {addr:?}"))?,
        ));
    }
    if let Some(rest) = spec.strip_prefix("iroh://") {
        let (endpoint, addr) = rest
            .split_once('@')
            .with_context(|| format!("iroh backend {spec:?} must be iroh://ID@HOST:PORT"))?;
        return Ok(Backend::Iroh {
            endpoint: endpoint
                .parse()
                .with_context(|| format!("parse Iroh endpoint id {endpoint:?}"))?,
            addr: addr
                .parse()
                .with_context(|| format!("parse Iroh direct address {addr:?}"))?,
        });
    }
    bail!("backend {spec:?} must start with tcp:// or iroh://")
}

fn normalize_sni(sni: &str) -> Result<String> {
    let sni = sni.trim().trim_end_matches('.').to_ascii_lowercase();
    if sni.is_empty()
        || sni.len() > 255
        || sni == "*"
        || sni.contains('*')
        || !sni.is_ascii()
        || sni
            .chars()
            .any(|c| c.is_ascii_whitespace() || c.is_ascii_control() || c == '/' || c == '\\')
    {
        bail!("invalid SNI route {sni:?}");
    }
    Ok(sni)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_and_iroh_routes() {
        let routes = Routes::parse(&[
            "confluence.soil=tcp://127.0.0.1:8443".to_string(),
            "crm.soil=iroh://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@10.0.0.5:4433".to_string(),
        ])
        .unwrap();
        assert_eq!(
            routes.resolve("CONFLUENCE.SOIL."),
            Some(&Backend::Tcp("127.0.0.1:8443".parse().unwrap()))
        );
        assert!(matches!(
            routes.resolve("crm.soil"),
            Some(Backend::Iroh { .. })
        ));
        assert!(routes.resolve("other.soil").is_none());
    }

    #[test]
    fn fails_closed_on_ambiguous_or_missing_specs() {
        assert!(Routes::parse(&["confluence.soil".to_string()]).is_err());
        assert!(Routes::parse(&["confluence.soil=udp://127.0.0.1:1".to_string()]).is_err());
        assert!(Routes::parse(&["*.soil=tcp://127.0.0.1:1".to_string()]).is_err());
        assert!(
            Routes::parse(&[
                "confluence.soil=tcp://127.0.0.1:1".to_string(),
                "Confluence.Soil=tcp://127.0.0.1:2".to_string(),
            ])
            .is_err()
        );
    }
}
