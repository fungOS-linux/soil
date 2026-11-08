use std::{fs, net::SocketAddr, path::Path, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use soil_confluence::spore::ConfluenceBus;
use soil_wasi_host::{
    GatewayRoute, HttpGatewayBuilder, HttpService, StaticCredentials, UnibusBus, WasiHostBuilder,
    WasiHostPolicy,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use unibus::client::ClientConfig;

#[derive(Parser)]
#[command(version, about = "Confluence webhook ingress on the Soil HTTP surface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the Confluence Spore on the Soil HTTP surface.
    Spore(SporeArgs),
}

#[derive(Parser)]
struct SporeArgs {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    #[arg(long)]
    unibus_config: PathBuf,
    /// Extra hub endpoints for sparse-hub failover, tried in order after the
    /// config's `hub_endpoint`. Repeat for each hub (pris, batty, studio, ...).
    #[arg(long)]
    unibus_hub: Vec<String>,
    #[arg(long, default_value = "default")]
    tenant: String,
    /// The compiled `wasi:http` Confluence component.
    #[arg(long)]
    spore: PathBuf,
    /// Public or internal hostnames this ingress answers for. Repeatable: one
    /// Spore can serve several names (an internal `.soil` name and a public
    /// `.myc` name) without a second process or a second Unibus session.
    #[arg(long, default_value = "confluence.soil")]
    host: Vec<String>,
    /// Spore identity reported to `fungos:http-context`.
    #[arg(long, default_value = "dev.fungos.confluence")]
    spore_name: String,
    /// `soil:creds` name bound to the provider webhook secret.
    #[arg(long, default_value = "confluence.webhook")]
    credential_name: String,
    #[arg(long, env = "CONFLUENCE_WEBHOOK_SECRET")]
    webhook_secret: Option<String>,
    #[arg(long, default_value_t = 64 * 1024)]
    max_body_bytes: usize,
    #[arg(long, default_value_t = 10_000)]
    request_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    match Cli::parse().command {
        Command::Spore(args) => serve_spore(args).await,
    }
}

async fn serve_spore(args: SporeArgs) -> Result<()> {
    let secret = non_empty(args.webhook_secret)
        .context("--webhook-secret or CONFLUENCE_WEBHOOK_SECRET is required")?;
    let wasm = fs::read(&args.spore)
        .with_context(|| format!("read Spore component {}", args.spore.display()))?;
    let config = load_unibus_config(&args.unibus_config)?;
    let inner = Arc::new(
        UnibusBus::connect_any(&config, &hub_endpoints(&config, &args.unibus_hub))
            .context("connect soil:unibus hub")?,
    );
    tracing::info!(hub = %inner.hub_endpoint(), "soil:unibus hub connected");
    let bus = Arc::new(ConfluenceBus::new(args.tenant.clone(), inner));

    let mut credentials = StaticCredentials::new();
    credentials.insert_hmac(
        args.credential_name.clone(),
        secret.into_bytes(),
        "verify provider webhooks",
    )?;
    let builder = WasiHostBuilder::new(WasiHostPolicy::default())
        .credential_provider(Arc::new(credentials))
        .bus(bus);
    let service = Arc::new(HttpService::from_bytes_named(
        args.spore_name.clone(),
        &wasm,
        builder,
    )?);

    let route = GatewayRoute::new([http::Method::POST], "/webhooks/")
        .context("grant POST /webhooks/")?
        .max_body_bytes(args.max_body_bytes);
    let mut gateway = HttpGatewayBuilder::new()
        .max_body_bytes(args.max_body_bytes)
        .request_timeout(Duration::from_millis(args.request_timeout_ms));
    for host in &args.host {
        gateway = gateway
            .vhost(
                host.clone(),
                args.spore_name.clone(),
                Arc::clone(&service),
                [route.clone()],
            )
            .with_context(|| format!("grant the Confluence vhost {host}"))?;
    }
    let gateway = gateway
        .build()
        .context("build the Confluence HTTP gateway")?;

    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind Spore HTTP listener {}", args.listen))?;
    tracing::info!(
        addr = %args.listen,
        hosts = %args.host.join(","),
        spore = %args.spore_name,
        tenant = %args.tenant,
        "confluence Spore ingress listening"
    );
    Arc::new(gateway).serve(listener).await
}

/// Ordered hub list: the config's `hub_endpoint` first, then `--unibus-hub`
/// extras in flag order. First to register wins; the order is the priority.
fn hub_endpoints(config: &ClientConfig, extra_hubs: &[String]) -> Vec<String> {
    let mut hubs = vec![config.hub_endpoint.clone()];
    hubs.extend(extra_hubs.iter().cloned());
    hubs
}

fn load_unibus_config(path: &Path) -> Result<ClientConfig> {
    let mut config: ClientConfig = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    config.resolve_token_file(path);
    Ok(config)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}
