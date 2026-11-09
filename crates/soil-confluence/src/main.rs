use std::{fs, net::IpAddr, net::SocketAddr, path::Path, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use soil_confluence::spore::ConfluenceBus;
use soil_confluence::webhooks::{Registry, Webhook};
use soil_wasi_host::{
    GatewayRoute, HttpGatewayBuilder, HttpService, OriginAuth, StaticCredentials, UnibusBus,
    WasiHostBuilder, WasiHostPolicy,
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
    /// Create, list, and revoke webhook endpoints.
    Webhook(WebhookArgs),
}

#[derive(Parser)]
struct WebhookArgs {
    #[command(subcommand)]
    command: WebhookCommand,
    /// Registry directory. One entry per hook, secrets beside them.
    #[arg(
        long,
        env = "CONFLUENCE_REGISTRY",
        default_value = ".confluence/webhooks"
    )]
    registry: PathBuf,
    /// Base URL the endpoints are published under, for display only.
    #[arg(long, default_value = "https://confluence.fpl.dev")]
    base_url: String,
}

#[derive(Subcommand)]
enum WebhookCommand {
    /// Create a hook. The secret is printed once and never again.
    Create {
        /// Human name, e.g. `github avery/soil`.
        #[arg(long)]
        name: String,
        /// Provider family, e.g. `github`.
        #[arg(long, default_value = "github")]
        provider: String,
        /// Unibus tenant stamped on every event.
        #[arg(long, default_value = "default")]
        tenant: String,
        /// Fixed path segment instead of a random one.
        #[arg(long)]
        slug: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List hooks. Secrets are never shown.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Revoke a hook and delete its secret.
    Revoke {
        slug: String,
        #[arg(long)]
        json: bool,
    },
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
    /// Registry of webhooks to accept. Every entry's secret is loaded by slug,
    /// so a hook created while the Spore is running needs a restart only to be
    /// addressable by its own slug, never to be rotated.
    #[arg(long)]
    webhook_registry: Option<PathBuf>,
    #[arg(long, default_value_t = 64 * 1024)]
    max_body_bytes: usize,
    /// Address of a TLS terminator allowed to assert a verified client
    /// certificate (Palisade, Thyra). Repeatable. When set, every other peer is
    /// refused: a direct hit on this listener is the origin bypass the policy
    /// exists to stop. Without it the transport asserts nothing and the Spore
    /// verifies each provider signature itself.
    #[arg(long)]
    trusted_terminator: Vec<IpAddr>,
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
        Command::Webhook(args) => run_webhook(args),
    }
}

fn run_webhook(args: WebhookArgs) -> Result<()> {
    let registry = Registry::new(&args.registry);
    match args.command {
        WebhookCommand::Create {
            name,
            provider,
            tenant,
            slug,
            json,
        } => {
            let created =
                registry.create(&name, &provider, &tenant, slug.as_deref(), unix_millis())?;
            let url = endpoint(&args.base_url, &created.webhook);
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "slug": created.webhook.slug,
                        "name": created.webhook.name,
                        "provider": created.webhook.provider,
                        "tenant": created.webhook.tenant,
                        "kind": created.webhook.kind,
                        "url": url,
                        "secret": created.secret,
                    })
                );
            } else {
                println!("url:    {url}");
                println!("secret: {}", created.secret);
                println!(
                    "
kind:   {} (tenant {})",
                    created.webhook.kind, created.webhook.tenant
                );
                println!("\nThe secret is not stored anywhere else. Save it now.");
            }
        }
        WebhookCommand::List { json } => {
            let hooks = registry.list()?;
            if json {
                let rows: Vec<_> = hooks
                    .iter()
                    .map(|hook| {
                        serde_json::json!({
                            "slug": hook.slug,
                            "name": hook.name,
                            "provider": hook.provider,
                            "tenant": hook.tenant,
                            "kind": hook.kind,
                            "url": endpoint(&args.base_url, hook),
                            "created_at_ms": hook.created_at_ms,
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(rows));
            } else if hooks.is_empty() {
                println!("no webhooks in {}", registry.dir().display());
            } else {
                for hook in hooks {
                    println!(
                        "{}  {}  {}",
                        endpoint(&args.base_url, &hook),
                        hook.name,
                        hook.tenant
                    );
                }
            }
        }
        WebhookCommand::Revoke { slug, json } => {
            let removed = registry.revoke(&slug)?;
            if json {
                println!("{}", serde_json::json!({"slug": slug, "revoked": removed}));
            } else if removed {
                println!("revoked {slug}");
            } else {
                println!("no webhook named {slug}");
            }
        }
    }
    Ok(())
}

fn endpoint(base_url: &str, webhook: &Webhook) -> String {
    format!(
        "{}/webhooks/{}/{}",
        base_url.trim_end_matches('/'),
        webhook.provider,
        webhook.slug
    )
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

async fn serve_spore(args: SporeArgs) -> Result<()> {
    ensure!(
        args.webhook_registry.is_some() || non_empty(args.webhook_secret.clone()).is_some(),
        "--webhook-registry, --webhook-secret, or CONFLUENCE_WEBHOOK_SECRET is required"
    );
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
    if let Some(secret) = non_empty(args.webhook_secret) {
        credentials.insert_hmac(
            args.credential_name.clone(),
            secret.into_bytes(),
            "verify provider webhooks",
        )?;
    }
    if let Some(path) = &args.webhook_registry {
        let registry = Registry::new(path);
        for (slug, secret) in registry.secrets()? {
            credentials.insert_hmac(slug, secret, "verify a registered webhook")?;
        }
    }
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
        .origin_auth(origin_auth(&args.trusted_terminator))
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
        verifier = origin_auth_label(&args.trusted_terminator),
        "confluence Spore ingress listening"
    );
    Arc::new(gateway).serve(listener).await
}

/// Transport truth for this listener.
///
/// A declared terminator is the only peer allowed to assert identity: it saw
/// the client certificate, this process did not. Without one, nothing about the
/// transport is asserted and each Spore verifies the provider's own signature.
fn origin_auth(peers: &[IpAddr]) -> OriginAuth {
    if peers.is_empty() {
        OriginAuth::ProviderSignature
    } else {
        OriginAuth::trusted_terminator(peers.iter().copied())
    }
}

fn origin_auth_label(peers: &[IpAddr]) -> &'static str {
    match origin_auth(peers) {
        OriginAuth::ProviderSignature => "provider-signature",
        OriginAuth::TrustedTerminator { .. } => "trusted-terminator",
    }
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
