//! Mycelium-authorized records peer host.
//!
//! There is no separate session issuer here. The local trust document is a
//! retained Mycelium `AccessRecord` grant whose scope commits to the exact
//! permit (both Iroh endpoint keys, message kinds, audience and payload bound).
//! `GrantVerifier` synthesizes a short-lived Unibus session from that signed
//! permit; a copied session JSON cannot widen it.

use std::{
    collections::BTreeSet,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use ed25519_dalek::SigningKey;
use fungos_records::{PeerEndpoint, Store};
use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};
use mycelium_peer_protocol::{AccessRecord, AccessStatement};
use serde::{Deserialize, Serialize};
use soil_sync_host::{RecordPeerHost, sync_records_with_transport};
use unibus::{
    DirectSessionVerifier, IrohPeerEndpoint, PeerAccessPolicy, PeerBinding, PeerCandidate,
};
use unibus_iroh::{
    PEER_ALPN,
    grant::{GrantVerifier, Permit, TrustDocument, permit_scope},
    transport::{IrohListener, IrohTransport, SessionConfig},
};

const DEFAULT_KINDS: &str = "records.sync.pull.v1,records.sync.ops.v1,records.sync.push.v1,records.sync.ack.v1,records.sync.error.v1";
const DEFAULT_MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

#[derive(Parser)]
#[command(version, about = "Mycelium-authorized Unibus Iroh records host")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a persistent Unibus endpoint key.
    Keygen {
        path: PathBuf,
        #[arg(long)]
        write: bool,
    },
    /// Print the Iroh endpoint id for a key file.
    EndpointId { key: PathBuf },
    /// Create a retained Mycelium-signed session permit.
    Permit(PermitArgs),
    /// Serve records over one `unibus/peer/1` endpoint.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Pull then push one records book over Iroh.
    Sync {
        #[arg(long)]
        config: PathBuf,
        /// Producer id from the config's `peers` list.
        #[arg(long)]
        peer: String,
        #[arg(long)]
        book: String,
    },
}

#[derive(Args)]
struct PermitArgs {
    /// Mycelium access signing key (raw 32 bytes or 64-char hex).
    #[arg(long)]
    key: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    audience: String,
    #[arg(long)]
    installation: String,
    #[arg(long)]
    initiator_producer: String,
    #[arg(long)]
    initiator_endpoint: String,
    #[arg(long)]
    responder_producer: String,
    #[arg(long)]
    responder_endpoint: String,
    #[arg(long, default_value = DEFAULT_KINDS)]
    kinds: String,
    #[arg(long, default_value_t = DEFAULT_MAX_PAYLOAD)]
    max_payload_bytes: u32,
    #[arg(long, default_value = "soil-records")]
    grant_id: String,
    #[arg(long, default_value = "soil-records")]
    principal: String,
    #[arg(long, default_value_t = 1)]
    serial: u64,
    #[arg(long)]
    not_before: Option<u64>,
    #[arg(long)]
    not_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SyncConfig {
    endpoint_key_file: PathBuf,
    #[serde(default = "default_bind_addr")]
    bind_addr: String,
    installation_id: String,
    local_producer: String,
    audience: String,
    trust_file: PathBuf,
    /// Explicit Mycelium access-issuer public keys. Never discovered remotely.
    issuers: Vec<String>,
    #[serde(default)]
    allowed_relays: Vec<String>,
    records_root: PathBuf,
    #[serde(default)]
    peers: Vec<PeerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerConfig {
    producer_id: String,
    endpoint_id: String,
    #[serde(default)]
    direct_addresses: Vec<SocketAddr>,
}

fn default_bind_addr() -> String {
    "0.0.0.0:0".into()
}

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-sync: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Keygen { path, write } => keygen(&path, write),
        Cmd::EndpointId { key } => {
            println!("{}", load_endpoint_key(&key)?.public());
            Ok(())
        }
        Cmd::Permit(args) => permit(args),
        Cmd::Serve { config } => tokio::runtime::Runtime::new()?.block_on(serve(config)),
        Cmd::Sync { config, peer, book } => {
            tokio::runtime::Runtime::new()?.block_on(sync(config, &peer, &book))
        }
    }
}

fn keygen(path: &Path, write: bool) -> anyhow::Result<()> {
    if path.exists() && !write {
        bail!(
            "{} already exists; pass --write to replace it",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let key = SecretKey::generate();
    fs::write(path, format!("{}\n", hex(&key.to_bytes())))?;
    set_owner_only(path)?;
    println!("endpoint_id={}", key.public());
    println!("key={}", path.display());
    Ok(())
}

fn permit(args: PermitArgs) -> anyhow::Result<()> {
    let now = now_s();
    let not_before = args.not_before.unwrap_or_else(|| now.saturating_sub(60));
    let not_after = args.not_after.unwrap_or_else(|| now + 30 * 24 * 60 * 60);
    if not_after <= not_before {
        bail!("--not-after must be after --not-before");
    }
    let kinds = args
        .kinds
        .split(',')
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if kinds.is_empty() {
        bail!("permit needs at least one message kind");
    }
    let permit = Permit {
        audience: args.audience,
        installation: args.installation,
        initiator: PeerBinding {
            producer_id: args.initiator_producer,
            key_binding: format!("iroh:{}", args.initiator_endpoint),
            candidates: vec![],
        },
        responder: PeerBinding {
            producer_id: args.responder_producer,
            key_binding: format!("iroh:{}", args.responder_endpoint),
            candidates: vec![],
        },
        kinds,
        max_payload_bytes: args.max_payload_bytes,
    };
    let scope = permit_scope(&permit)?;
    let statement = AccessStatement::Grant {
        grant_id: args.grant_id,
        principal: args.principal,
        serial: args.serial,
        roles: vec![],
        scopes: vec![scope.clone()],
        unix_users: vec![],
        ssh_public_keys: vec![],
        oidc_audiences: vec![],
        oidc_ssh_key_exchange: false,
        not_before,
        not_after,
    };
    let key = load_access_key(&args.key)?;
    let grant = AccessRecord::sign(&key, statement)?;
    grant.verify().map_err(anyhow::Error::msg)?;
    let document = TrustDocument {
        grant,
        revocations: vec![],
        permit,
    };
    if let Some(parent) = args.out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.out, serde_json::to_vec_pretty(&document)?)?;
    set_owner_only(&args.out)?;
    println!("issuer={}", document.grant.signer);
    println!("scope={scope}");
    println!("trust={}", args.out.display());
    Ok(())
}

async fn serve(config_path: PathBuf) -> anyhow::Result<()> {
    let config = read_config(&config_path)?;
    let endpoint = bind_endpoint(&config).await?;
    let verifier = make_verifier(&config)?;
    let session = session_config(&config, verifier);
    let listener = IrohListener::new(endpoint.clone(), session);
    let store = Store::open(&config.records_root, &config.records_root.join("device_id"))?;
    eprintln!("soil-sync: producer={}", config.local_producer);
    eprintln!("soil-sync: endpoint_id={}", endpoint.id());
    for address in endpoint.addr().ip_addrs() {
        eprintln!("soil-sync: direct_address={address}");
    }
    let host = RecordPeerHost::new(
        listener,
        store,
        PeerEndpoint {
            installation_id: config.installation_id.clone(),
            producer_id: config.local_producer.clone(),
        },
    );
    host.run().await
}

async fn sync(config_path: PathBuf, peer: &str, book: &str) -> anyhow::Result<()> {
    let config = read_config(&config_path)?;
    let peer_config = config
        .peers
        .iter()
        .find(|p| p.producer_id == peer)
        .ok_or_else(|| anyhow::anyhow!("peer `{peer}` is not configured"))?;
    let verifier = make_verifier(&config)?;
    let assertion = verifier.assertion(now_s())?;
    let endpoint = bind_endpoint(&config).await?;
    let transport = IrohTransport::new(endpoint.clone(), session_config(&config, verifier));
    let candidate = PeerCandidate {
        peer_id: peer_config.producer_id.clone(),
        site: None,
        endpoints: vec![],
        iroh_endpoints: vec![IrohPeerEndpoint {
            endpoint_id: peer_config.endpoint_id.clone(),
            direct_addresses: peer_config.direct_addresses.clone(),
            relay_urls: vec![],
        }],
        expires_at_unix_s: now_s() + 60,
    };
    let mut store = Store::open(&config.records_root, &config.records_root.join("device_id"))?;
    let outcome = sync_records_with_transport(
        &transport,
        &candidate,
        &assertion,
        &mut store,
        book,
        &PeerEndpoint {
            installation_id: config.installation_id.clone(),
            producer_id: config.local_producer.clone(),
        },
    )
    .await?;
    println!(
        "synced {book} with {}: pulled {}, pushed {}, local={:?}, remote={:?}",
        peer_config.producer_id, outcome.pulled, outcome.pushed, outcome.local, outcome.remote
    );
    endpoint.close().await;
    Ok(())
}

fn read_config(path: &Path) -> anyhow::Result<SyncConfig> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn make_verifier(config: &SyncConfig) -> anyhow::Result<Arc<GrantVerifier>> {
    let issuers = config.issuers.iter().cloned().collect::<BTreeSet<_>>();
    Ok(Arc::new(GrantVerifier::new(
        config.trust_file.clone(),
        issuers,
    )?))
}

fn session_config(config: &SyncConfig, verifier: Arc<GrantVerifier>) -> SessionConfig {
    let verifier_dyn: Arc<dyn DirectSessionVerifier> = verifier.clone();
    let policy: Arc<dyn PeerAccessPolicy> = verifier;
    SessionConfig {
        verifier: verifier_dyn,
        audience: config.audience.clone(),
        installation_id: config.installation_id.clone(),
        local_producer: config.local_producer.clone(),
        allowed_relays: config.allowed_relays.iter().cloned().collect(),
        access_policy: Some(policy),
        endpoint_identity: None,
    }
}

async fn bind_endpoint(config: &SyncConfig) -> anyhow::Result<Endpoint> {
    let key = load_endpoint_key(&config.endpoint_key_file)?;
    let address: SocketAddr = config
        .bind_addr
        .parse()
        .with_context(|| format!("invalid bind_addr `{}`", config.bind_addr))?;
    let builder = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::Disabled)
        .bind_addr(address)?
        .alpns(vec![PEER_ALPN.as_bytes().to_vec()]);
    builder.bind().await.context("bind Iroh endpoint")
}

fn load_endpoint_key(path: &Path) -> anyhow::Result<SecretKey> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let raw = if bytes.len() == 32 {
        bytes
    } else {
        let text = std::str::from_utf8(&bytes).context("endpoint key is not UTF-8")?;
        decode_hex(text.trim()).map_err(anyhow::Error::msg)?
    };
    let raw: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow::anyhow!("endpoint key must be 32 bytes"))?;
    Ok(SecretKey::from_bytes(&raw))
}

fn load_access_key(path: &Path) -> anyhow::Result<SigningKey> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let raw = if bytes.len() == 32 {
        bytes
    } else {
        let text = std::str::from_utf8(&bytes).context("access key is not UTF-8")?;
        decode_hex(text.trim()).map_err(anyhow::Error::msg)?
    };
    let raw: [u8; 32] = raw
        .try_into()
        .map_err(|_| anyhow::anyhow!("access key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&raw))
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("hex key has odd length".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&text[index..index + 2], 16)
                .map_err(|_| "hex key contains a non-hex digit".into())
        })
        .collect()
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_: &Path) -> anyhow::Result<()> {
    Ok(())
}
