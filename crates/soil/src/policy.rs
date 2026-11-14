//! Host-owned policy for WASI networking and HTTP.
//!
//! WASI describes the shape of the capability. Soil decides whether a Spore may
//! use it. These types are deliberately conservative: the default policy denies
//! outbound HTTP, DNS, TCP, and UDP.

use std::collections::BTreeSet;
#[cfg(feature = "wasi")]
use std::net::SocketAddr;

use anyhow::{Context, Result};
use ipnet::IpNet;
use url::Url;
#[cfg(feature = "wasi")]
use wasmtime_wasi::WasiCtxBuilder;
#[cfg(feature = "wasi")]
use wasmtime_wasi::sockets::SocketAddrUse;

/// Outbound `wasi:http` policy.
///
/// Origins are normalized to `scheme://host:port`. An empty set denies every
/// outgoing request.
#[derive(Clone, Debug, Default)]
pub struct HttpPolicy {
    origins: BTreeSet<String>,
    max_field_size: Option<usize>,
}

impl HttpPolicy {
    /// Build a policy from absolute origins such as `https://example.com`.
    pub fn new(origins: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut policy = Self::default();
        for origin in origins {
            policy.allow_origin(&origin)?;
        }
        Ok(policy)
    }

    /// Allow one absolute origin.
    pub fn allow_origin(&mut self, origin: &str) -> Result<()> {
        self.origins.insert(normalize_origin(origin)?);
        Ok(())
    }

    /// Iterate over the normalized allowed origins.
    pub fn origins(&self) -> impl Iterator<Item = &str> {
        self.origins.iter().map(String::as_str)
    }

    /// Override the `wasi:http` field-map limit.
    pub fn set_max_field_size(&mut self, limit: usize) {
        self.max_field_size = Some(limit);
    }

    #[cfg(feature = "wasi")]
    pub(crate) fn max_field_size(&self) -> usize {
        self.max_field_size.unwrap_or(128 * 1024)
    }

    /// Whether an outgoing request URI is allowed by this policy.
    pub fn allows_uri(&self, uri: &http::Uri) -> bool {
        let Ok(url) = Url::parse(&uri.to_string()) else {
            return false;
        };
        let Some(origin) = origin_of_url(&url) else {
            return false;
        };
        self.origins.contains(&origin)
    }
}

/// `wasi:sockets` policy.
///
/// Each use of a socket address is checked independently. Empty lists deny the
/// corresponding operation. DNS is disabled unless explicitly enabled.
#[derive(Clone, Debug, Default)]
pub struct NetworkPolicy {
    tcp_connect: Vec<IpNet>,
    tcp_bind: Vec<IpNet>,
    tcp_accept: Vec<IpNet>,
    udp_send: Vec<IpNet>,
    udp_receive: Vec<IpNet>,
    udp_bind: Vec<IpNet>,
    allow_dns: bool,
}

impl NetworkPolicy {
    /// Allow TCP connections to a CIDR.
    pub fn allow_tcp_connect(&mut self, cidr: &str) -> Result<()> {
        self.tcp_connect.push(parse_cidr(cidr)?);
        Ok(())
    }

    /// Allow TCP binds to a CIDR.
    pub fn allow_tcp_bind(&mut self, cidr: &str) -> Result<()> {
        self.tcp_bind.push(parse_cidr(cidr)?);
        Ok(())
    }

    /// Allow inbound TCP accepts from a CIDR.
    pub fn allow_tcp_accept(&mut self, cidr: &str) -> Result<()> {
        self.tcp_accept.push(parse_cidr(cidr)?);
        Ok(())
    }

    /// Allow UDP sends to a CIDR.
    pub fn allow_udp_send(&mut self, cidr: &str) -> Result<()> {
        self.udp_send.push(parse_cidr(cidr)?);
        Ok(())
    }

    /// Allow UDP receives from a CIDR.
    pub fn allow_udp_receive(&mut self, cidr: &str) -> Result<()> {
        self.udp_receive.push(parse_cidr(cidr)?);
        Ok(())
    }

    /// Allow UDP binds to a CIDR.
    pub fn allow_udp_bind(&mut self, cidr: &str) -> Result<()> {
        self.udp_bind.push(parse_cidr(cidr)?);
        Ok(())
    }

    /// Allow `wasi:sockets/ip-name-lookup`.
    pub fn allow_dns(&mut self, allow: bool) {
        self.allow_dns = allow;
    }

    #[cfg(feature = "wasi")]
    pub(crate) fn apply(&self, builder: &mut WasiCtxBuilder) {
        let tcp = !(self.tcp_connect.is_empty()
            && self.tcp_bind.is_empty()
            && self.tcp_accept.is_empty());
        let udp =
            !(self.udp_send.is_empty() && self.udp_receive.is_empty() && self.udp_bind.is_empty());
        builder.allow_tcp(tcp);
        builder.allow_udp(udp);
        builder.allow_ip_name_lookup(self.allow_dns);
        let policy = self.clone();
        builder.socket_addr_check(move |addr, use_| {
            let allowed = policy.allows(addr, use_);
            Box::pin(async move { allowed })
        });
    }

    #[cfg(feature = "wasi")]
    fn allows(&self, addr: SocketAddr, use_: SocketAddrUse) -> bool {
        let ranges = match use_ {
            SocketAddrUse::TcpConnect => &self.tcp_connect,
            SocketAddrUse::TcpBind => &self.tcp_bind,
            SocketAddrUse::TcpListen => &self.tcp_bind,
            SocketAddrUse::TcpAccept => &self.tcp_accept,
            SocketAddrUse::UdpSend => &self.udp_send,
            SocketAddrUse::UdpReceive => &self.udp_receive,
            SocketAddrUse::UdpBind => &self.udp_bind,
        };
        ranges.iter().any(|range| range.contains(&addr.ip()))
    }
}

/// Combined host policy for a WASI component.
#[derive(Clone, Debug, Default)]
pub struct WasiHostPolicy {
    pub http: HttpPolicy,
    pub network: NetworkPolicy,
}

fn parse_cidr(cidr: &str) -> Result<IpNet> {
    cidr.parse::<IpNet>()
        .with_context(|| format!("parse CIDR {cidr}"))
}

fn normalize_origin(origin: &str) -> Result<String> {
    let url = Url::parse(origin).with_context(|| format!("parse origin {origin}"))?;
    origin_of_url(&url).ok_or_else(|| anyhow::anyhow!("origin {origin} must be http or https"))
}

fn origin_of_url(url: &Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    Some(format!(
        "{}://{}:{port}",
        url.scheme(),
        host.to_ascii_lowercase()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_policy_is_origin_scoped() {
        let mut policy = HttpPolicy::new(["https://example.com".to_string()]).unwrap();
        assert!(policy.allows_uri(&"https://example.com/path".parse().unwrap()));
        assert!(!policy.allows_uri(&"https://example.com.evil/path".parse().unwrap()));
        assert!(!policy.allows_uri(&"http://example.com/path".parse().unwrap()));
        policy.allow_origin("http://127.0.0.1:8080").unwrap();
        assert!(policy.allows_uri(&"http://127.0.0.1:8080/".parse().unwrap()));
    }

    #[test]
    fn network_policy_denies_by_default() {
        let policy = NetworkPolicy::default();
        assert!(!policy.allows("127.0.0.1:80".parse().unwrap(), SocketAddrUse::TcpConnect));
    }

    #[test]
    fn network_policy_is_use_specific() {
        let mut policy = NetworkPolicy::default();
        policy.allow_tcp_connect("10.0.0.0/8").unwrap();
        assert!(policy.allows("10.2.3.4:443".parse().unwrap(), SocketAddrUse::TcpConnect));
        assert!(!policy.allows("10.2.3.4:443".parse().unwrap(), SocketAddrUse::TcpBind));
        assert!(!policy.allows("127.0.0.1:443".parse().unwrap(), SocketAddrUse::TcpConnect));
    }
}
