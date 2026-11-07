use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use iroh::{EndpointId, SecretKey};
use serde_json::Value;
use soil_confluence::{
    AppState, ConfluenceConfig, UnibusPublisher, bind_iroh, push_iroh, relay_router, router,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use unibus::client::ClientConfig;

#[derive(Parser)]
#[command(version, about = "Minimal Confluence webhook ingress for Unibus")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Accept webhooks and publish Confluence events to Unibus.
    Serve(ServeArgs),
    /// Send one webhook frame to a Confluence Iroh listener.
    Push(PushArgs),
    /// Accept local HTTP and forward it to a Confluence Iroh listener.
    Relay(RelayArgs),
}

#[derive(Parser)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    #[arg(long)]
    unibus_config: PathBuf,
    #[arg(long, default_value = "default")]
    tenant: String,
    #[arg(long, env = "CONFLUENCE_WEBHOOK_SECRET")]
    webhook_secret: Option<String>,
    #[arg(long, env = "CONFLUENCE_INGRESS_TOKEN")]
    ingress_token: Option<String>,
    #[arg(long)]
    allow_unsigned: bool,
    #[arg(long)]
    iroh: bool,
    #[arg(long, default_value = "127.0.0.1:0")]
    iroh_bind: SocketAddr,
    #[arg(long)]
    iroh_key: Option<PathBuf>,
    #[arg(long)]
    iroh_peer: Vec<String>,
    #[arg(long)]
    iroh_allow_any: bool,
}

#[derive(Parser)]
struct RelayArgs {
    #[arg(long, default_value = "127.0.0.1:8898")]
    listen: SocketAddr,
    #[arg(long)]
    endpoint_id: String,
    #[arg(long)]
    addr: SocketAddr,
}

#[derive(Parser)]
struct PushArgs {
    #[arg(long)]
    endpoint_id: String,
    #[arg(long)]
    addr: SocketAddr,
    #[arg(long, default_value = "local")]
    provider: String,
    #[arg(long)]
    body: String,
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
        Command::Push(args) => push(args).await,
        Command::Relay(args) => relay(args).await,
    }
}

async fn serve(args: ServeArgs) -> Result<()> {
    let publisher = Arc::new(UnibusPublisher::connect(load_unibus_config(
        &args.unibus_config,
    )?)?);
    let webhook_secret = non_empty(args.webhook_secret).map(String::into_bytes);
    let ingress_token = non_empty(args.ingress_token);
    ensure!(
        webhook_secret.is_some() || ingress_token.is_some() || args.allow_unsigned,
        "configure --webhook-secret-env, --ingress-token-env, or --allow-unsigned"
    );
    let state = Arc::new(AppState::new(
        ConfluenceConfig {
            tenant: args.tenant,
            webhook_secret,
            ingress_token,
            allow_unsigned: args.allow_unsigned,
        },
        publisher,
    ));

    let mut _iroh_endpoint = None;
    if args.iroh {
        let key = match &args.iroh_key {
            Some(path) => load_secret_key(path)?,
            None => SecretKey::generate(),
        };
        let peers = args
            .iroh_peer
            .iter()
            .map(|peer| peer.parse::<EndpointId>())
            .collect::<Result<Vec<_>, _>>()
            .context("parse --iroh-peer")?;
        let endpoint = bind_iroh(
            state.clone(),
            key,
            args.iroh_bind,
            peers,
            args.iroh_allow_any,
        )
        .await?;
        println!("iroh_endpoint_id={}", endpoint.id());
        for addr in endpoint.bound_sockets() {
            println!("iroh_addr={addr}");
        }
        _iroh_endpoint = Some(endpoint);
    }

    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind confluence HTTP listener {}", args.listen))?;
    tracing::info!(addr = %args.listen, "confluence webhook ingress listening");
    axum::serve(listener, router(state))
        .await
        .context("serve confluence HTTP")
}

async fn push(args: PushArgs) -> Result<()> {
    let endpoint_id: EndpointId = args.endpoint_id.parse().context("parse endpoint id")?;
    let body = read_body(&args.body)?;
    let response = push_iroh(endpoint_id, args.addr, args.provider, body).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    ensure!(response.status == 202, "confluence rejected webhook");
    Ok(())
}

async fn relay(args: RelayArgs) -> Result<()> {
    let endpoint_id: EndpointId = args.endpoint_id.parse().context("parse endpoint id")?;
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind confluence relay {}", args.listen))?;
    tracing::info!(addr = %args.listen, "confluence HTTP-to-Iroh relay listening");
    axum::serve(listener, relay_router(endpoint_id, args.addr))
        .await
        .context("serve confluence relay")
}

fn load_unibus_config(path: &Path) -> Result<ClientConfig> {
    let mut config: ClientConfig = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    config.resolve_token_file(path);
    Ok(config)
}

fn load_secret_key(path: &Path) -> Result<SecretKey> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let bytes = hex::decode(text.trim()).context("decode iroh key hex")?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("iroh key must be 32 bytes"))?;
    Ok(SecretKey::from_bytes(&bytes))
}

fn read_body(value: &str) -> Result<Value> {
    let text = match value.strip_prefix('@') {
        Some(path) => fs::read_to_string(path).with_context(|| format!("read {path}"))?,
        None => value.to_string(),
    };
    let body: Value = serde_json::from_str(&text).context("parse webhook body")?;
    ensure!(body.is_object(), "webhook body must be a JSON object");
    Ok(body)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}
