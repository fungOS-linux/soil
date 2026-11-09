//! HTTP over Iroh.
//!
//! Two halves of one idea: a name should resolve to an *identity*, and the
//! transport should be chosen at connect time. `serve` turns an Iroh ALPN into
//! a local HTTP endpoint; `proxy` turns a local HTTP endpoint into an Iroh
//! connection. Neither knows anything about HTTP semantics — no parsing, no
//! rewriting — so the Spore behind it sees exactly the request a client sent,
//! and a vhost header still means what it means over TCP.
//!
//! Both sides fail closed: an unknown ALPN, an endpoint id that does not match
//! the lease that named it, or a dead stream is a refusal, never a fallback to
//! plaintext.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, endpoint::presets};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about = "HTTP over Iroh: serve adapter and loopback proxy")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Accept Iroh streams and hand each one to a local HTTP service.
    Serve(ServeArgs),
    /// Accept local HTTP and carry each connection to an Iroh endpoint.
    Proxy(ProxyArgs),
}

#[derive(Parser)]
struct ServeArgs {
    /// File holding the endpoint's secret key. Created if absent, so the
    /// endpoint id survives a restart and the lease that names it stays valid.
    #[arg(long)]
    key: PathBuf,
    /// Iroh bind address. 0.0.0.0:0 picks a random UDP port and publishes what
    /// it chose, which is what direct dialling needs.
    #[arg(long, default_value = "0.0.0.0:0")]
    bind: SocketAddr,
    /// ALPN this adapter answers on.
    #[arg(long, default_value = "soil/http/1")]
    alpn: String,
    /// Local HTTP service to forward each stream to.
    #[arg(long)]
    forward: SocketAddr,
    /// Peers allowed to dial. Empty accepts any peer that knows the endpoint id;
    /// a non-empty list fails closed on everything else.
    #[arg(long)]
    allow: Vec<String>,
}

#[derive(Parser)]
struct ProxyArgs {
    /// Loopback address clients connect to. Point the `.myc` name at this.
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: SocketAddr,
    /// Endpoint id to dial.
    #[arg(long)]
    endpoint: String,
    /// Direct address to try for that endpoint. Repeatable.
    #[arg(long)]
    addr: Vec<SocketAddr>,
    #[arg(long, default_value = "soil/http/1")]
    alpn: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    match Cli::parse().command {
        Command::Serve(args) => serve(args).await,
        Command::Proxy(args) => proxy(args).await,
    }
}

async fn serve(args: ServeArgs) -> Result<()> {
    let key = load_or_create_key(&args.key)?;
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .alpns(vec![args.alpn.clone().into_bytes()])
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr(args.bind)
        .context("invalid Iroh bind address")?
        .bind()
        .await
        .context("bind Iroh endpoint")?;

    let alpn = args.alpn.into_bytes();
    let allow: Vec<EndpointId> = args
        .allow
        .iter()
        .map(|id| id.parse())
        .collect::<Result<_, _>>()
        .context("parse --allow endpoint id")?;

    tracing::info!(
        endpoint = %endpoint.id(),
        alpn = %String::from_utf8_lossy(&alpn),
        forward = %args.forward,
        allow = allow.len(),
        "iroh http adapter serving"
    );

    while let Some(incoming) = endpoint.accept().await {
        let forward = args.forward;
        let allow = allow.clone();
        let alpn = alpn.clone();
        tokio::spawn(async move {
            match handle_incoming(incoming, forward, &allow, &alpn).await {
                Ok(peer) => tracing::info!(%peer, "iroh stream forwarded"),
                Err(error) => tracing::warn!(error = %format!("{error:#}"), "iroh stream refused"),
            }
        });
    }
    Ok(())
}

async fn handle_incoming(
    incoming: iroh::endpoint::Incoming,
    forward: SocketAddr,
    allow: &[EndpointId],
    alpn: &[u8],
) -> Result<EndpointId> {
    let connection = incoming.await.context("complete Iroh handshake")?;
    let peer = connection.remote_id();
    if !allow.is_empty() && !allow.contains(&peer) {
        anyhow::bail!("peer {peer} is not in the allow list");
    }
    ensure!(
        connection.alpn() == alpn,
        "unexpected ALPN {:?}",
        String::from_utf8_lossy(connection.alpn())
    );
    let (send, recv) = connection
        .accept_bi()
        .await
        .context("accept bidirectional stream")?;
    let upstream = TcpStream::connect(forward)
        .await
        .with_context(|| format!("connect {forward}"))?;
    let (mut read, mut write) = upstream.into_split();
    pump(recv, send, &mut read, &mut write).await;
    Ok(peer)
}

async fn proxy(args: ProxyArgs) -> Result<()> {
    let target: EndpointId = args.endpoint.parse().context("parse --endpoint")?;
    ensure!(!args.addr.is_empty(), "at least one --addr is required");
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .context("bind proxy Iroh endpoint")?;
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind {}", args.listen))?;
    tracing::info!(
        listen = %args.listen,
        target = %target,
        addr = ?args.addr,
        "iroh http proxy listening"
    );

    loop {
        let (client, peer) = listener.accept().await.context("accept local client")?;
        let endpoint = endpoint.clone();
        let addr = args.addr.clone();
        let alpn = args.alpn.clone().into_bytes();
        tokio::spawn(async move {
            match proxy_one(client, &endpoint, target, &addr, &alpn).await {
                Ok(()) => tracing::info!(%peer, "proxied over iroh"),
                Err(error) => tracing::warn!(%peer, error = %format!("{error:#}"), "proxy failed"),
            }
        });
    }
}

async fn proxy_one(
    client: TcpStream,
    endpoint: &Endpoint,
    target: EndpointId,
    addrs: &[SocketAddr],
    alpn: &[u8],
) -> Result<()> {
    let mut connection = None;
    let mut last = None;
    for addr in addrs {
        match endpoint
            .connect(EndpointAddr::new(target).with_ip_addr(*addr), alpn)
            .await
        {
            Ok(connection_handle) => {
                connection = Some(connection_handle);
                break;
            }
            Err(error) => last = Some(error),
        }
    }
    let connection = match connection {
        Some(connection) => connection,
        None => anyhow::bail!(
            "no direct address reached {target}: {}",
            last.map(|error| error.to_string())
                .unwrap_or_else(|| "no addresses".into())
        ),
    };
    let (send, recv) = connection
        .open_bi()
        .await
        .context("open bidirectional stream")?;
    let (mut read, mut write) = client.into_split();
    pump(recv, send, &mut read, &mut write).await;
    Ok(())
}

/// One HTTP exchange, byte for byte, until either side closes.
async fn pump<R, S>(mut recv: R, mut send: S, read: &mut tokio::net::tcp::OwnedReadHalf, write: &mut tokio::net::tcp::OwnedWriteHalf)
where
    R: AsyncRead + Unpin,
    S: AsyncWrite + Unpin,
{
    // Both directions run at once. `copy` returns when either half ends, and
    // the client half ending early is normal: it sent its request and is
    // waiting. So a finished request grants the response a bounded grace
    // period instead of tearing the stream down mid-body.
    {
        let request = tokio::io::copy(read, &mut send);
        let response = tokio::io::copy(&mut recv, write);
        tokio::pin!(request, response);
        tokio::select! {
            _ = &mut response => {}
            _ = &mut request => {
                let grace = std::time::Duration::from_secs(5);
                let _ = tokio::time::timeout(grace, &mut response).await;
            }
        }
    }
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut send).await;
}

fn load_or_create_key(path: &Path) -> Result<SecretKey> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let bytes = hex::decode(text.trim()).context("decode endpoint key")?;
        ensure!(bytes.len() == 32, "endpoint key must be 32 bytes");
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        return Ok(SecretKey::from_bytes(&key));
    }
    let key = SecretKey::generate();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, hex::encode(key.to_bytes()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(key)
}
