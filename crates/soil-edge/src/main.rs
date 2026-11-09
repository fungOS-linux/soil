//! Bring up the Soil L4 SNI edge.
//!
//! ```sh
//! soil-edge \
//!   --listen 0.0.0.0:443 \
//!   --route confluence.soil=tcp://127.0.0.1:8443 \
//!   --route crm.soil=iroh://<endpoint-id>@10.0.0.5:4433
//! ```

use std::{process::ExitCode, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use soil_edge::{Edge, EdgeConfig, Routes};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-edge: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let routes = Routes::parse(&args.route)?;
    if routes.is_empty() {
        bail!("at least one --route SNI=BACKEND is required");
    }
    let mut config = EdgeConfig::new(routes);
    config.alpn = args.alpn.into_bytes();
    config.iroh_bind = args.iroh_bind;
    config.iroh_bind = args.iroh_bind;
    config.max_client_hello = args.max_client_hello;
    config.handshake_timeout = Duration::from_millis(args.handshake_timeout_ms);
    config.connect_timeout = Duration::from_millis(args.connect_timeout_ms);

    for (sni, backend) in config.routes.iter() {
        println!("soil-edge: {sni} -> {backend}");
    }
    let edge = Arc::new(Edge::bind(config).await?);
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind edge listener {}", args.listen))?;
    println!("soil-edge: listening on {}", args.listen);
    edge.serve(listener).await
}

#[derive(Parser)]
#[command(version, about = "Soil L4 SNI edge: TLS passthrough, no body access")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:443")]
    listen: std::net::SocketAddr,
    /// SNI=BACKEND; backend is tcp://HOST:PORT or iroh://ID@HOST:PORT.
    #[arg(long = "route", value_name = "SNI=BACKEND", required = true)]
    route: Vec<String>,
    #[arg(long, default_value = "confluence/tls/1")]
    alpn: String,
    /// Local address for the outbound Iroh endpoint.
    #[arg(long, default_value = "0.0.0.0:0")]
    iroh_bind: std::net::SocketAddr,
    #[arg(long, default_value_t = 64 * 1024)]
    max_client_hello: usize,
    #[arg(long, default_value_t = 10_000)]
    handshake_timeout_ms: u64,
    #[arg(long, default_value_t = 10_000)]
    connect_timeout_ms: u64,
}
