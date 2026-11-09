//! The L4 edge: parse SNI, pick a granted backend, and copy bytes.
//!
//! The edge never completes a TLS handshake. It buffers one ClientHello to
//! read the SNI, writes those exact bytes to the chosen backend, then copies in
//! both directions. Provider TLS terminates at Soil, so the edge sees only
//! ciphertext after the first flight.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail};
use iroh::{Endpoint, EndpointAddr, endpoint::presets};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _, copy, copy_bidirectional},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tracing::{debug, info, warn};

use crate::{
    client_hello::{HelloParse, MAX_CLIENT_HELLO, parse_server_name},
    route::{Backend, Routes},
};

#[derive(Clone, Debug)]
pub struct EdgeConfig {
    pub routes: Routes,
    pub alpn: Vec<u8>,
    pub iroh_bind: SocketAddr,
    pub max_client_hello: usize,
    pub handshake_timeout: Duration,
    pub connect_timeout: Duration,
}

impl EdgeConfig {
    pub fn new(routes: Routes) -> Self {
        Self {
            routes,
            alpn: b"confluence/tls/1".to_vec(),
            iroh_bind: "0.0.0.0:0".parse().expect("static Iroh bind address"),
            max_client_hello: MAX_CLIENT_HELLO,
            handshake_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
        }
    }
}

pub struct Edge {
    config: EdgeConfig,
    iroh: Option<Endpoint>,
}

impl Edge {
    /// Build the edge and its Iroh endpoint, if a route needs one.
    pub async fn bind(config: EdgeConfig) -> Result<Self> {
        if config.routes.is_empty() {
            bail!("edge has no SNI routes");
        }
        if config.max_client_hello < 5 || config.max_client_hello > MAX_CLIENT_HELLO {
            bail!("client hello budget must be between 5 and {MAX_CLIENT_HELLO} bytes");
        }
        if config.alpn.is_empty() || config.alpn.len() > 255 {
            bail!("ALPN must be between 1 and 255 bytes");
        }
        if config.handshake_timeout.is_zero() || config.connect_timeout.is_zero() {
            bail!("edge timeouts must be positive");
        }
        let needs_iroh = config
            .routes
            .iter()
            .any(|(_, backend)| matches!(backend, Backend::Iroh { .. }));
        let iroh = if needs_iroh {
            Some(
                Endpoint::builder(presets::Minimal)
                    .relay_mode(iroh::RelayMode::Disabled)
                    .bind_addr(config.iroh_bind)
                    .context("invalid Iroh bind address")?
                    .bind()
                    .await
                    .context("bind edge Iroh endpoint")?,
            )
        } else {
            None
        };
        Ok(Self { config, iroh })
    }

    /// Serve until the listener fails or the task is cancelled.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        loop {
            let (client, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    warn!(%error, "edge accept failed");
                    continue;
                }
            };
            let edge = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(error) = edge.handle(client, peer).await {
                    debug!(%peer, error = %format!("{error:#}"), "edge connection closed");
                }
            });
        }
    }

    async fn handle(&self, mut client: TcpStream, peer: SocketAddr) -> Result<()> {
        let (hello, sni) = timeout(
            self.config.handshake_timeout,
            read_client_hello(&mut client, self.config.max_client_hello),
        )
        .await
        .context("client hello timed out")??;
        let Some(sni) = sni else {
            bail!("client hello has no SNI");
        };
        let Some(backend) = self.config.routes.resolve(&sni) else {
            bail!("no route for SNI {sni}");
        };
        info!(%peer, %sni, backend = %backend, "routing TLS connection");

        match backend {
            Backend::Tcp(addr) => {
                let mut upstream = timeout(self.config.connect_timeout, TcpStream::connect(addr))
                    .await
                    .with_context(|| format!("connect {addr} timed out"))??;
                upstream
                    .write_all(&hello)
                    .await
                    .context("replay client hello to TCP backend")?;
                copy_bidirectional(&mut client, &mut upstream)
                    .await
                    .context("copy TLS bytes")?;
            }
            Backend::Iroh {
                endpoint: target,
                addr,
            } => {
                let endpoint = self
                    .iroh
                    .as_ref()
                    .context("Iroh backend configured but no Iroh endpoint")?;
                let connection = timeout(
                    self.config.connect_timeout,
                    endpoint.connect(
                        EndpointAddr::new(*target).with_ip_addr(*addr),
                        &self.config.alpn,
                    ),
                )
                .await
                .with_context(|| format!("connect Iroh endpoint {target} timed out"))??;
                let (mut send, mut recv) = connection
                    .open_bi()
                    .await
                    .context("open Iroh bidirectional stream")?;
                send.write_all(&hello)
                    .await
                    .context("replay client hello over Iroh")?;
                debug!(%peer, %sni, "client hello replayed over Iroh");

                let (mut read, mut write) = client.into_split();
                let up = async {
                    let copied = copy(&mut read, &mut send).await;
                    let _ = send.finish();
                    copied
                };
                let down = async {
                    let copied = copy(&mut recv, &mut write).await;
                    // The upstream half may still be waiting for client EOF.
                    // Close the client's write side now so `read_to_end` on the
                    // other end sees a clean end of response instead of waiting
                    // for the TCP connection to drop.
                    let _ = write.shutdown().await;
                    debug!(%peer, %sni, bytes = ?copied, "Iroh downstream finished");
                    copied
                };
                let (up, down) = tokio::try_join!(up, down).context("copy TLS bytes over Iroh")?;
                debug!(%peer, %sni, up, down, "edge Iroh connection closed");
            }
        }
        Ok(())
    }
}

async fn read_client_hello(
    stream: &mut TcpStream,
    max: usize,
) -> Result<(Vec<u8>, Option<String>)> {
    let mut hello = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match parse_server_name(&hello).context("parse client hello")? {
            HelloParse::Complete(sni) => return Ok((hello, sni)),
            HelloParse::NeedMore => {
                if hello.len() >= max {
                    bail!("client hello exceeds {max} bytes");
                }
                let read = stream.read(&mut chunk).await.context("read client hello")?;
                if read == 0 {
                    bail!("connection closed before client hello completed");
                }
                hello.extend_from_slice(&chunk[..read]);
            }
        }
    }
}
