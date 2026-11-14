//! `soil spore run`: host any `.spore` on the names it claims.
//!
//! One launcher, no product code. The Spore decides where it answers (its
//! `[http]` table) and what it may do (`[grants]`); this process decides nothing
//! except the listener, and refuses to start when two Spores want one name.
//!
//! The claim is routing, not privilege: a Spore that claims `contacts.myc` gets
//! requests routed to it and still cannot read a file, open a socket or touch a
//! record it was not granted. That split is why self-assertion is safe.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use soil_spore::{Manifest, OriginAuth as ClaimedAuth};
use ::soil::{
    GatewayRoute, GeodeCredentials, HttpGatewayBuilder, HttpService, OriginAuth, UnibusBus,
    WasiHostBuilder, WasiHostPolicy,
};
use tokio::net::TcpListener;

/// How a run is asked for.
#[derive(Debug)]
pub struct Run {
    pub listen: Option<SocketAddr>,
    pub unibus_config: Option<PathBuf>,
    pub hubs: Vec<String>,
    pub max_body: Option<usize>,
    pub trusted_terminators: Vec<IpAddr>,
    pub state: Option<PathBuf>,
    /// `geoded`'s socket. When set, the Spore resolves credentials through a
    /// lease instead of anything in this process.
    pub geode_socket: Option<PathBuf>,
    /// `geode://<realm>/<account>/`, prefixed to whatever name a Spore asks for.
    pub geode_prefix: String,
    pub geode_purpose: String,
    pub geode_ttl_secs: u64,
    /// Serve a web bridge here, so a browser can offer `soil:http`.
    pub bridge: Option<SocketAddr>,
    /// How long to wait for a page to offer `soil:http` before starting anyway.
    pub bridge_wait: Duration,
    /// Origins this Spore may reach through `soil:http`.
    pub allow_origins: Vec<String>,
}

pub fn parse(args: &[String]) -> Result<(PathBuf, Run)> {
    // `soil spore run <path>`: the verb is `run`, and the path follows it.
    let args = match args.first().map(String::as_str) {
        Some("run") => &args[1..],
        _ => args,
    };
    let Some(spore) = args.first().map(PathBuf::from) else {
        bail!("usage: soil spore run <spore-dir|package.spore> [options]");
    };
    let mut run = Run {
        listen: None,
        unibus_config: None,
        hubs: Vec::new(),
        max_body: None,
        trusted_terminators: Vec::new(),
        state: None,
        geode_socket: None,
        geode_prefix: "geode://local/default/".into(),
        // Verification is what an ingress does with a credential; signing is a
        // separate capability and must be asked for explicitly.
        geode_purpose: "verify".into(),
        geode_ttl_secs: 600,
        bridge: None,
        bridge_wait: Duration::ZERO,
        allow_origins: Vec::new(),
    };
    let mut index = 1;
    while index < args.len() {
        let value = |index: usize| {
            args.get(index)
                .cloned()
                .with_context(|| format!("{} needs a value", args[index - 1]))
        };
        match args[index].as_str() {
            "--listen" => {
                run.listen = Some(value(index + 1)?.parse().context("--listen is addr:port")?);
                index += 2;
            }
            "--unibus-config" => {
                run.unibus_config = Some(PathBuf::from(value(index + 1)?));
                index += 2;
            }
            "--hub" => {
                run.hubs.push(value(index + 1)?);
                index += 2;
            }
            "--max-body" => {
                run.max_body = Some(value(index + 1)?.parse().context("--max-body is bytes")?);
                index += 2;
            }
            "--trusted-terminator" => {
                run.trusted_terminators.push(
                    value(index + 1)?
                        .parse()
                        .context("--trusted-terminator is an IP")?,
                );
                index += 2;
            }
            "--state" => {
                run.state = Some(PathBuf::from(value(index + 1)?));
                index += 2;
            }
            "--geode-socket" => {
                run.geode_socket = Some(PathBuf::from(value(index + 1)?));
                index += 2;
            }
            "--geode-prefix" => {
                run.geode_prefix = value(index + 1)?;
                index += 2;
            }
            "--geode-purpose" => {
                run.geode_purpose = value(index + 1)?;
                index += 2;
            }
            "--geode-ttl-secs" => {
                run.geode_ttl_secs = value(index + 1)?
                    .parse()
                    .context("--geode-ttl-secs is seconds")?;
                index += 2;
            }
            "--bridge" => {
                run.bridge = Some(value(index + 1)?.parse().context("--bridge is addr:port")?);
                index += 2;
            }
            "--bridge-wait" => {
                run.bridge_wait = Duration::from_secs(
                    value(index + 1)?
                        .parse()
                        .context("--bridge-wait is seconds")?,
                );
                index += 2;
            }
            "--allow-origin" => {
                run.allow_origins.push(value(index + 1)?);
                index += 2;
            }
            other => bail!("unknown option `{other}`"),
        }
    }
    Ok((spore, run))
}

/// Run a Spore until the listener closes.
pub fn run(args: &[String]) -> Result<()> {
    // A host that cannot say what it is doing is a host nobody can operate. The
    // default is `info` for the host crates, so a plain launch is not silent; an
    // operator can widen or narrow it with `RUST_LOG`, exactly as they can for
    // every other soil service.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,soil=info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let (path, run) = parse(args)?;
    let root = Spore::resolve(&path)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("start the async runtime")?;
    runtime.block_on(serve(root, run))
}

/// A Spore unpacked and ready to host.
struct Spore {
    root: PathBuf,
    manifest: Manifest,
    /// Set when the package was unpacked to a temporary directory, so it is
    /// cleaned up when the process ends rather than accumulating.
    transient: Option<PathBuf>,
}

impl Spore {
    /// A directory, or a `.spore` package verified and unpacked.
    fn resolve(path: &Path) -> Result<Self> {
        if path.is_dir() {
            let manifest =
                Manifest::load(path).map_err(|error| anyhow::anyhow!(error.to_string()))?;
            return Ok(Self {
                root: path.to_path_buf(),
                manifest,
                transient: None,
            });
        }
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        // Verify before unpacking: a package that fails its own digest check
        // must never reach a temp directory, let alone a listener.
        soil_spore::verify(&bytes).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let transient = temp_root(path);
        let _ = std::fs::remove_dir_all(&transient);
        std::fs::create_dir_all(&transient)
            .with_context(|| format!("create {}", transient.display()))?;
        soil_spore::unpack(&bytes, &transient)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let manifest =
            Manifest::load(&transient).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Self {
            root: transient.clone(),
            manifest,
            transient: Some(transient),
        })
    }
}

impl Drop for Spore {
    fn drop(&mut self) {
        if let Some(transient) = &self.transient {
            let _ = std::fs::remove_dir_all(transient);
        }
    }
}

async fn serve(spore: Spore, run: Run) -> Result<()> {
    let claimed = spore.manifest.http.as_ref().with_context(|| {
        format!(
            "{} claims no [http], so there is nothing for the launcher to host",
            spore.manifest.spore.id
        )
    })?;
    claim_hosts(
        &spore.manifest.spore.id,
        &claimed.hosts,
        run.state.as_deref(),
    )?;

    // The face that answers is the one the `[http]` table names, falling back
    // to the package's own module. A Spore whose window and endpoint are two
    // artifacts says so here; the gateway instantiates only what it calls.
    let module = claimed
        .module
        .clone()
        .or_else(|| {
            spore
                .manifest
                .module
                .as_ref()
                .map(|module| module.path.clone())
        })
        .with_context(|| {
            format!(
                "{} claims a name but has no module to answer with",
                spore.manifest.spore.id
            )
        })?;
    let wasm =
        std::fs::read(spore.root.join(&module)).with_context(|| format!("read module {module}"))?;

    let mut builder = WasiHostBuilder::new(WasiHostPolicy::default());
    if let Some(socket) = &run.geode_socket {
        builder = builder.credential_provider(Arc::new(GeodeCredentials::new(
            socket,
            run.geode_prefix.clone(),
            run.geode_purpose.clone(),
            Duration::from_secs(run.geode_ttl_secs),
        )));
    }
    if let Some(bus) = bus(&spore.manifest, &run)? {
        builder = builder.bus(bus);
    }

    // The browser, when one connects. The Spore still runs here; `soil:http`
    // reaches the network through a page that volunteered the interface, and
    // falls back to this process's own socket when none did.
    let mut _bridge_server = None;
    if let Some(listen) = run.bridge {
        let policy = ::soil::http_host::HttpPolicy::new(
            run.allow_origins.iter().cloned(),
            8 * 1024 * 1024,
            Duration::from_secs(30),
        )
        .context("soil:http policy")?;
        let grants = soil_web_bridge::GrantSet::from_manifest(&spore.manifest);
        let bridge = Arc::new(
            soil_web_bridge::Bridge::new(
                spore.manifest.spore.id.clone(),
                grants,
                soil_web_bridge::Registry::new(),
            )
            .with_auth(soil_web_bridge::Auth::loopback_only()),
        );
        let server = bridge
            .clone()
            .serve(listen)
            .context("start the web bridge")?;
        println!("soil: bridge on http://{}", server.addr());
        if run.allow_origins.is_empty() {
            println!("soil: no --allow-origin, so soil:http reaches nothing");
        } else {
            println!("soil: soil:http may reach {}", run.allow_origins.join(", "));
        }
        if !run.bridge_wait.is_zero() {
            let deadline = tokio::time::Instant::now() + run.bridge_wait;
            while !bridge.provides("soil:http") && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if !bridge.provides("soil:http") {
                println!("soil: no page offered soil:http; fetches use this process's network");
            }
        }
        let transport = Arc::new(::soil::http_host::PreferProvider::new(
            Arc::new(soil_web_bridge::SessionTransport::new(bridge)),
            Arc::new(::soil::http_host::DirectTransport),
        ));
        builder = builder.with_http(::soil::HttpCapability::new(policy, transport));
        _bridge_server = Some(server);
    }
    let service = Arc::new(
        HttpService::from_bytes_named(spore.manifest.spore.id.clone(), &wasm, builder)
            .with_context(|| format!("load {}", spore.manifest.spore.id))?,
    );

    let max_body = run.max_body.unwrap_or(claimed.max_body);
    let mut gateway = HttpGatewayBuilder::new()
        .max_body_bytes(max_body)
        .origin_auth(origin_auth(&claimed.origin_auth, &run.trusted_terminators)?)
        .request_timeout(Duration::from_secs(30));
    for host in &claimed.hosts {
        let mut routes = Vec::new();
        for route in &claimed.routes {
            routes.push(
                GatewayRoute::new(
                    route.methods.iter().map(|method| match method.as_str() {
                        "GET" => http::Method::GET,
                        "HEAD" => http::Method::HEAD,
                        "POST" => http::Method::POST,
                        "PUT" => http::Method::PUT,
                        "PATCH" => http::Method::PATCH,
                        "DELETE" => http::Method::DELETE,
                        "OPTIONS" => http::Method::OPTIONS,
                        // Validation already refused anything else.
                        _ => http::Method::GET,
                    }),
                    &route.path,
                )
                .with_context(|| format!("grant {} {}", route.methods.join(","), route.path))?
                .max_body_bytes(max_body),
            );
        }
        gateway = gateway
            .vhost(
                host.clone(),
                spore.manifest.spore.id.clone(),
                Arc::clone(&service),
                routes,
            )
            .with_context(|| format!("grant the vhost {host}"))?;
    }
    let gateway = gateway.build().context("build the HTTP gateway")?;

    let listen = run
        .listen
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8787)));
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    tracing::info!(
        addr = %listen,
        spore = %spore.manifest.spore.id,
        hosts = %claimed.hosts.join(","),
        "spore listening"
    );
    Arc::new(gateway).serve(listener).await
}

/// The bus, when the Spore is granted one.
///
/// A Spore that observes or publishes needs a hub; one that does not must not
/// be handed a connection it never asked for.
fn bus(manifest: &Manifest, run: &Run) -> Result<Option<Arc<dyn ::soil::Bus>>> {
    let wants = manifest
        .grants
        .keys()
        .any(|grant| grant.starts_with("soil:unibus"));
    if !wants {
        return Ok(None);
    }
    let config = run.unibus_config.as_ref().with_context(|| {
        format!(
            "{} is granted soil:unibus but no --unibus-config was given",
            manifest.spore.id
        )
    })?;
    let mut base: unibus::client::ClientConfig = serde_json::from_slice(
        &std::fs::read(config).with_context(|| format!("read {}", config.display()))?,
    )
    .with_context(|| format!("parse {}", config.display()))?;
    base.resolve_token_file(config);
    let mut endpoints = vec![base.hub_endpoint.clone()];
    endpoints.extend(run.hubs.iter().cloned());
    let bus = UnibusBus::connect_any(&base, &endpoints)?;
    Ok(Some(Arc::new(bus)))
}

fn origin_auth(claimed: &ClaimedAuth, terminators: &[IpAddr]) -> Result<OriginAuth> {
    match claimed {
        ClaimedAuth::ProviderSignature => Ok(OriginAuth::ProviderSignature),
        ClaimedAuth::TrustedTerminator if terminators.is_empty() => bail!(
            "this Spore trusts a terminator, so --trusted-terminator must name it; \
             otherwise every peer can assert an identity nobody verified"
        ),
        ClaimedAuth::TrustedTerminator => {
            Ok(OriginAuth::trusted_terminator(terminators.iter().copied()))
        }
    }
}

/// Where a claim lives between processes, so two Spores cannot share a name.
fn claim_dir(state: Option<&Path>) -> PathBuf {
    state
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            std::env::var_os("SOIL_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                        .join(".local/state/soil")
                })
        })
        .join("hosts")
}

/// Take the claimed names, or refuse.
///
/// A name is exclusive. Two Spores on one vhost do not round-robin: whichever
/// bound last would answer, and the other would look deployed while serving
/// nothing - the failure that is hardest to see from outside.
fn claim_hosts(spore: &str, hosts: &[String], state: Option<&Path>) -> Result<()> {
    let dir = claim_dir(state);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let overrides = overrides();
    for host in hosts {
        if let Some(pinned) = overrides.get(host)
            && pinned != spore
        {
            bail!(
                "{host} is pinned to {pinned} by the host override file; refusing to let {spore} take it"
            );
        }
        let claim = dir.join(claim_name(host));
        if let Ok(body) = std::fs::read_to_string(&claim) {
            let mut parts = body.split_whitespace();
            let owner = parts.next().unwrap_or_default();
            let pid = parts.next().and_then(|pid| pid.parse::<u32>().ok());
            if owner != spore && pid.is_some_and(alive) {
                bail!(
                    "{host} is already served by {owner} (pid {}); a name answers from one Spore at a time",
                    pid.unwrap_or_default()
                );
            }
        }
        std::fs::write(&claim, format!("{spore} {}\n", std::process::id()))
            .with_context(|| format!("claim {host}"))?;
    }
    Ok(())
}

fn claim_name(host: &str) -> String {
    host.chars()
        .map(|character| {
            if character == '.' || character == '*' {
                '_'
            } else {
                character
            }
        })
        .collect()
}

/// Whether a pid is still running, without trusting the claim file's age.
fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
            || std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Host-level overrides: `[hosts]` in `~/.config/soil/hosts.toml`.
fn overrides() -> BTreeMap<String, String> {
    let path = std::env::var_os("SOIL_HOSTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".config/soil/hosts.toml")
        });
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        tracing::warn!(
            path = %path.display(),
            "host override file is not valid TOML; ignoring it"
        );
        return BTreeMap::new();
    };
    value
        .get("hosts")
        .and_then(|hosts| hosts.as_table())
        .map(|table| {
            table
                .iter()
                .filter_map(|(host, owner)| {
                    owner.as_str().map(|owner| (host.clone(), owner.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn temp_root(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "spore".into());
    std::env::temp_dir().join(format!("soil-spore-{}-{name}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(http: &str) -> String {
        format!(
            "schema = 1\n[spore]\nid = \"dev.fungos.test\"\nname = \"test\"\nversion = \"0.1.0\"\nlicense = \"MIT\"\nsummary = \"s\"\n[engine]\nsoil = \">=0.1\"\ntarget = \"wasm32-wasip2\"\n[module]\npath = \"module.wasm\"\n[ui]\nentry = \"ui/main.lua\"\n[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n{http}"
        )
    }

    #[test]
    fn the_claim_is_the_manifest_and_nothing_else() {
        let parsed = Manifest::parse(&manifest(
            "[http]\nhosts = [\"contacts.myc\"]\nroutes = [{ methods = [\"GET\"], path = \"/\" }]\n",
        ))
        .expect("manifest");
        let http = parsed.http.expect("a claim");
        assert_eq!(http.hosts, vec!["contacts.myc"]);
    }

    #[test]
    fn a_second_spore_cannot_take_a_live_name() {
        let state = std::env::temp_dir().join(format!(
            "soil-claims-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        claim_hosts(
            "dev.fungos.contacts",
            &["contacts.myc".into()],
            Some(&state),
        )
        .expect("first claim wins");
        // Same Spore re-claiming is fine: a restart must not be a conflict.
        claim_hosts(
            "dev.fungos.contacts",
            &["contacts.myc".into()],
            Some(&state),
        )
        .expect("re-claim by the same Spore");
        let stolen = claim_hosts("dev.fungos.other", &["contacts.myc".into()], Some(&state));
        assert!(stolen.is_err(), "a name answers from one Spore at a time");
        let _ = std::fs::remove_dir_all(state);
    }
}
