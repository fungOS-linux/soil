//! Host side of soil:net — the resolver and a TCP reachability probe.
//!
//! The guest names a host or a mesh node (`<node>.<site>.myc`) and gets data
//! back. Sockets, resolver configuration and packets stay here: the guest can
//! ask "what is this name" and "is this service up", never "connect me to
//! 1.2.3.4:22".
//!
//! Resolution runs on a throwaway thread because `getaddrinfo` has no timeout of
//! its own. The caller's budget wins; an abandoned lookup is left to finish and
//! its answer dropped, which is the only way to bound a resolver that hangs.

use std::{
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::soil::net::net;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Timeout,
    Invalid,
    Unreachable,
}

pub struct Resolved {
    pub addresses: Vec<SocketAddr>,
    pub elapsed: Duration,
}

pub fn resolve(name: &str, timeout: Duration) -> Result<Resolved, Error> {
    lookup(name, 0, timeout)
}

/// The port belongs in the lookup: a `SocketAddr` carries it, and connecting to
/// an address resolved with port 0 would dial port 0.
fn lookup(name: &str, port: u16, timeout: Duration) -> Result<Resolved, Error> {
    if name.is_empty() || name.len() > 253 || name.contains(['/', '\0', ' ']) {
        return Err(Error::Invalid);
    }
    let start = Instant::now();
    let target = name.to_string();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let answer = (target.as_str(), port)
            .to_socket_addrs()
            .map(|addresses| addresses.collect::<Vec<_>>());
        // The receiver is gone when the lookup lost its budget; that is fine.
        let _ = sender.send(answer);
    });
    let addresses = match receiver.recv_timeout(timeout) {
        Ok(Ok(addresses)) => addresses,
        Ok(Err(_)) => return Err(Error::NotFound),
        Err(_) => return Err(Error::Timeout),
    };
    if addresses.is_empty() {
        return Err(Error::NotFound);
    }
    Ok(Resolved {
        addresses,
        elapsed: start.elapsed(),
    })
}

pub fn probe(name: &str, port: u16, timeout: Duration) -> Result<(SocketAddr, Duration), Error> {
    if port == 0 {
        return Err(Error::Invalid);
    }
    let resolved = lookup(name, port, timeout)?;
    let deadline = Instant::now() + timeout;
    for address in resolved.addresses {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Timeout);
        }
        let start = Instant::now();
        if let Ok(stream) = TcpStream::connect_timeout(&address, left) {
            drop(stream);
            return Ok((address, start.elapsed()));
        }
    }
    Err(Error::Unreachable)
}

impl net::Host for crate::Host {
    fn resolve(&mut self, name: String, timeout_ms: u32) -> Result<net::Resolution, net::Error> {
        let timeout = Duration::from_millis(u64::from(timeout_ms.max(1)));
        let resolved = resolve(&name, timeout).map_err(map_error)?;
        Ok(net::Resolution {
            name,
            addresses: resolved
                .addresses
                .iter()
                .map(|address| net::Address {
                    family: if address.is_ipv4() {
                        net::Family::Ipv4
                    } else {
                        net::Family::Ipv6
                    },
                    addr: address.ip().to_string(),
                })
                .collect(),
            elapsed_ms: resolved.elapsed.as_millis() as u64,
        })
    }

    fn probe(
        &mut self,
        name: String,
        port: u16,
        timeout_ms: u32,
    ) -> Result<net::Reachability, net::Error> {
        let timeout = Duration::from_millis(u64::from(timeout_ms.max(1)));
        let (address, rtt) = probe(&name, port, timeout).map_err(map_error)?;
        Ok(net::Reachability {
            name,
            addr: address.ip().to_string(),
            port,
            rtt_ms: rtt.as_millis() as u64,
        })
    }
}

fn map_error(error: Error) -> net::Error {
    match error {
        Error::NotFound => net::Error::NotFound,
        Error::Timeout => net::Error::Timeout,
        Error::Invalid => net::Error::Invalid,
        Error::Unreachable => net::Error::Unreachable,
    }
}
