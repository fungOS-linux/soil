//! `soil-spore`: build and verify Spore packages.
//!
//! Soil owns make and verify. Publishing is a tar over IPFS; trust roots come
//! from Mycelium (a bound PGP key) or an external keyring.

use soil_spore::manual::{Manual, read_prose, render_info};
use soil_spore::publisher::TrustedPublisher;
use soil_spore::{
    Error, Manifest, check, identity::Identity, lease, pack, publish_blockers, read_package, sign,
    sign_principal, verify, verify_signature, write_lock,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A manifest, plus the files when the source was a packed package.
type Loaded = (Manifest, Option<BTreeMap<String, Vec<u8>>>);

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-spore: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), Error> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(Error::Invalid(usage()));
    };
    let rest = &args[1..];

    match command {
        "--version" | "-V" | "version" => {
            println!("soil-spore {VERSION}");
        }
        "--help" | "-h" | "help" => {
            println!("{}", usage());
        }
        "check" => {
            let publish = rest.iter().any(|arg| arg == "--publish");
            let paths: Vec<&String> = rest.iter().filter(|arg| *arg != "--publish").collect();
            let root = match paths.as_slice() {
                [path] => PathBuf::from(path.as_str()),
                _ => return Err(Error::Invalid(usage())),
            };
            let lock = check(&root)?;
            println!(
                "ok {} {} {} files {}",
                lock.id,
                lock.version,
                lock.files.len(),
                lock.digest
            );
            if publish {
                let manifest = Manifest::load(&root)?;
                let blockers = publish_blockers(&manifest);
                if !blockers.is_empty() {
                    for blocker in &blockers {
                        eprintln!("listing: {blocker}");
                    }
                    return Err(Error::Invalid(format!(
                        "{} listing requirement(s) unmet",
                        blockers.len()
                    )));
                }
                println!("ok listing requirements met; human review is still required");
            }
        }
        "lock" => {
            let root = one_path(rest)?;
            let lock = write_lock(&root)?;
            println!(
                "wrote {} ({})",
                root.join("spore.lock").display(),
                lock.digest
            );
        }
        "announce" => announce(rest)?,
        "man" => {
            let (path, width) = man_args(rest)?;
            let (manifest, files) = load(&path)?;
            let prose = match &files {
                Some(files) => manifest
                    .docs
                    .as_ref()
                    .and_then(|docs| docs.man.as_ref())
                    .and_then(|man| files.get(man))
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
                None => read_prose(&path, &manifest)?,
            };
            print!("{}", Manual::from_manifest(&manifest, prose).render(width));
        }
        "info" => {
            let root = one_path(rest)?;
            let (manifest, _) = load(&root)?;
            print!("{}", render_info(&manifest));
        }
        "pack" => {
            let (root, output) = pack_args(rest)?;
            let bytes = pack(&root)?;
            let path = output.unwrap_or_else(|| {
                PathBuf::from(format!(
                    "{}-{}.spore",
                    root.file_name()
                        .map_or_else(|| "spore".into(), |n| n.to_string_lossy().into_owned()),
                    "package"
                ))
            });
            std::fs::write(&path, &bytes)
                .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
            let (_, lock) = verify(&bytes)?;
            println!("{} {} bytes {}", path.display(), bytes.len(), lock.digest);
        }
        "verify" => {
            let path = one_path(rest)?;
            let bytes = std::fs::read(&path)
                .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
            let (manifest, lock) = verify(&bytes)?;
            println!(
                "ok {} {} {} files {}",
                manifest.spore.id,
                manifest.spore.version,
                lock.files.len(),
                lock.digest
            );
        }
        "digest" => {
            let root = one_path(rest)?;
            let manifest = Manifest::load(&root)?;
            let lock = soil_spore::compute_lock(&root, &manifest)?;
            println!("{}", lock.digest);
        }
        "sign" => {
            let (package, key) = optional_arg(rest, "--key")?;
            let principal = named_text(rest, "--principal");
            let socket = named_arg(rest, "--socket");
            let fingerprint = named_text(rest, "--fingerprint");
            let signature = signature_path(&package);
            if let Some(principal) = principal {
                if key.is_some() {
                    return Err(Error::Invalid(
                        "--principal and --key are mutually exclusive".into(),
                    ));
                }
                sign_principal(
                    &package,
                    &signature,
                    &principal,
                    socket.as_deref(),
                    fingerprint.as_deref(),
                )?;
            } else {
                sign(&package, &signature, key.as_deref())?;
            }
            println!("{}", signature.display());
        }
        "verify-signature" => {
            let (package, key) = optional_arg(rest, "--key")?;
            let keyring = named_arg(rest, "--keyring");
            let signature = signature_path(&package);
            verify_signature(
                &package,
                &signature,
                keyring.as_deref().map(Path::new),
                key.as_deref(),
            )?;
            println!("ok signature {}", signature.display());
        }
        other => {
            return Err(Error::Invalid(format!(
                "unknown command {other:?}\n{}",
                usage()
            )));
        }
    }
    Ok(())
}

fn usage() -> String {
    "usage: soil-spore check <dir> [--publish]\n       soil-spore lock <dir>\n       soil-spore digest <dir>\n       soil-spore man <dir|package.spore> [--width N]\n       soil-spore info <dir|package.spore>\n       soil-spore pack <dir> [-o out.spore]\n       soil-spore verify <package.spore>\n       soil-spore sign <package.spore> [--key ID | --principal LABEL [--socket PATH] [--fingerprint FP]]\n       soil-spore verify-signature <package.spore> [--keyring FILE] [--key ID]\n       soil-spore announce <dir> --endpoint URI [--home DIR] [--out FILE] [--every SECS] [--once] [--unsigned-dev]\n       soil-spore --version\n\nThe node id is read from --home's enrollment (default ~/.mycelium), never typed:\nthat is what makes a restarted node announce as itself.".into()
}

/// Read a Spore from a directory or a packed package.
fn load(path: &Path) -> Result<Loaded, Error> {
    if path.is_dir() {
        return Ok((Manifest::load(path)?, None));
    }
    let bytes =
        std::fs::read(path).map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    let (manifest, _, files) = read_package(&bytes)?;
    Ok((manifest, Some(files)))
}

fn man_args(args: &[String]) -> Result<(PathBuf, usize), Error> {
    let mut path = None;
    let mut width = 80;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--width" {
            let Some(value) = args.get(index + 1) else {
                return Err(Error::Invalid(usage()));
            };
            width = value
                .parse()
                .map_err(|_| Error::Invalid(format!("bad width {value:?}")))?;
            index += 2;
            continue;
        }
        if path.is_some() {
            return Err(Error::Invalid(usage()));
        }
        path = Some(PathBuf::from(&args[index]));
        index += 1;
    }
    path.map(|path| (path, width))
        .ok_or_else(|| Error::Invalid(usage()))
}

/// Publish this Spore's `[anycast]` claim as a Mycelium service lease.
///
/// The claim is not permission: a real announcement has to be authorised by the
/// package's publisher grant, and the trust view that decides that lives in
/// Mycelium. Until that adapter exists this refuses unless `--unsigned-dev` is
/// passed, so a local test cannot be mistaken for a fleet grant.
///
/// Nothing here is told who the node is. The identity is read from the
/// enrollment Mycelium already performed, which is what lets one command run
/// from cloud-init on a machine nobody has logged into.
fn announce(args: &[String]) -> Result<(), Error> {
    let options = Announce::parse(args)?;
    let manifest = Manifest::load(&options.root)?;
    let Some(anycast) = &manifest.anycast else {
        return Err(Error::Invalid(
            "this Spore declares no [anycast] claim, so it has no name to publish".into(),
        ));
    };
    if !options.unsigned {
        return Err(Error::Invalid(
            "refusing to announce without a publisher grant; Mycelium supplies the trust view. \
             Pass --unsigned-dev for a local test"
                .into(),
        ));
    }

    // A renewal interval at or beyond the lease is a node that leaves the pool
    // between every beat. Refuse it rather than publish a flapping replica.
    let every = renewal(options.every, anycast.ttl_secs)?;

    let identity = Identity::read(&options.home)?;
    let lock = check(&options.root)?;
    let publisher = TrustedPublisher {
        principal: "unsigned-dev".into(),
        grant_id: "none".into(),
        key_id: "none".into(),
    };
    if options.endpoints.iter().all(|endpoint| loopback(endpoint)) {
        eprintln!(
            "soil-spore: every endpoint is loopback, so no other node can dial this lease; \
             publish the mesh address instead"
        );
    }

    let service_id = lease::service_id(&manifest, &identity)?;
    let stop = stop::install().map_err(Error::Invalid)?;
    loop {
        let document = lease::document(
            &manifest,
            &lock,
            &publisher,
            &identity,
            &options.endpoints,
            unix_now(),
        )?;
        publish(&options.out, &document)?;
        println!(
            "announced {} as {service_id} on {} for {}s, renewing every {every}s",
            identity.name.as_deref().unwrap_or(&identity.node_id),
            options.endpoints.join(","),
            anycast.ttl_secs
        );
        if options.once || !stop::sleep(&stop, every) {
            break;
        }
    }

    if options.once {
        return Ok(());
    }
    // Stopped on purpose: withdraw now instead of leaving a lease that outlives
    // the process. A supervisor that stops this unit is replacing the node, and
    // a replacement must not queue behind a record for the node it replaced.
    if withdraw(&options.out, &service_id)? {
        println!("drained {service_id}; this node is out of the pool now");
    }
    Ok(())
}

/// Everything `announce` needs from its command line.
///
/// Parsed apart from the work so that refusing a bad interval, or a missing
/// node home, is testable without a daemon, a network, or a lease file.
struct Announce {
    root: PathBuf,
    home: PathBuf,
    out: PathBuf,
    endpoints: Vec<String>,
    every: Option<u64>,
    once: bool,
    unsigned: bool,
}

impl Announce {
    fn parse(args: &[String]) -> Result<Self, Error> {
        let Some(root) = args.first().map(PathBuf::from) else {
            return Err(Error::Invalid(usage()));
        };
        let mut home = None;
        let mut out = None;
        let mut endpoints: Vec<String> = Vec::new();
        let mut every = None;
        let mut once = false;
        let mut unsigned = false;
        let mut index = 1;
        while index < args.len() {
            let value = |index: usize| {
                args.get(index)
                    .cloned()
                    .ok_or_else(|| Error::Invalid(format!("{} needs a value", args[index - 1])))
            };
            match args[index].as_str() {
                "--home" => {
                    home = Some(PathBuf::from(value(index + 1)?));
                    index += 2;
                }
                "--endpoint" => {
                    endpoints.push(value(index + 1)?);
                    index += 2;
                }
                "--out" => {
                    out = Some(PathBuf::from(value(index + 1)?));
                    index += 2;
                }
                "--every" => {
                    every = Some(
                        value(index + 1)?
                            .parse()
                            .map_err(|_| Error::Invalid("--every is seconds".into()))?,
                    );
                    index += 2;
                }
                "--once" => {
                    once = true;
                    index += 1;
                }
                "--unsigned-dev" => {
                    unsigned = true;
                    index += 1;
                }
                other => return Err(Error::Invalid(format!("unknown option {other}"))),
            }
        }
        if endpoints.is_empty() {
            return Err(Error::Invalid("at least one --endpoint is required".into()));
        }
        let home = home.unwrap_or_else(mycelium_home);
        Ok(Self {
            root,
            out: out.unwrap_or_else(|| home.join("application-services.json")),
            home,
            endpoints,
            every,
            once,
            unsigned,
        })
    }
}

fn mycelium_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".mycelium")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// How often to renew, given whatever the operator asked for.
///
/// The default leaves three chances per lease. An explicit interval may be
/// anything shorter — a busy node may want to refresh every 30 s — but never
/// equal to or longer than the lease, because that node is out of the pool
/// for part of every cycle and looks like a flapping replica to the router.
fn renewal(requested: Option<u64>, ttl_secs: u64) -> Result<u64, Error> {
    let every = requested.unwrap_or((ttl_secs / 3).max(1));
    if every == 0 || every >= ttl_secs {
        return Err(Error::Invalid(format!(
            "--every {every}s is not shorter than the {ttl_secs}s lease; this node would \
             expire between renewals"
        )));
    }
    Ok(every)
}

/// Whether a hint dials back into whichever host reads it.
fn loopback(endpoint: &str) -> bool {
    let Some((_, rest)) = endpoint.split_once("://") else {
        return false;
    };
    let host = if let Some(rest) = rest.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        rest.split(':').next().unwrap_or_default()
    };
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// Every observation currently claimed by anything on this node.
fn observations(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|existing| existing["observations"].as_array().cloned())
        .unwrap_or_default()
}

/// Replace our own observation in the lease file and leave everyone else's
/// alone: `myceliumd` reads one document, so two Spores on a node must merge
/// rather than clobber.
fn publish(path: &Path, document: &serde_json::Value) -> Result<(), Error> {
    let ours: Vec<serde_json::Value> = document["observations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let owned: Vec<String> = ours
        .iter()
        .filter_map(|observation| observation["service_id"].as_str().map(str::to_owned))
        .collect();

    let mut kept: Vec<serde_json::Value> = observations(path)
        .into_iter()
        .filter(|observation| {
            observation["service_id"]
                .as_str()
                .is_none_or(|id| !owned.iter().any(|ours| ours == id))
        })
        .collect();
    kept.extend(ours);
    store(path, &kept)
}

/// Drop one observation, and report whether it was there to drop.
///
/// The daemon may keep the record until it expires, in which case withdrawing
/// is no worse than leaving it; it can only ever end the lease sooner.
fn withdraw(path: &Path, service_id: &str) -> Result<bool, Error> {
    let mut kept = observations(path);
    let before = kept.len();
    kept.retain(|observation| observation["service_id"].as_str() != Some(service_id));
    store(path, &kept)?;
    Ok(kept.len() != before)
}

fn store(path: &Path, observations: &[serde_json::Value]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| Error::Io(format!("{}: {error}", parent.display())))?;
    }
    let body = serde_json::json!({"schema_version": 1, "observations": observations});
    // Write beside the target and rename, so a reader never sees half a lease.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(&body).map_err(|error| Error::Io(error.to_string()))?,
    )
    .map_err(|error| Error::Io(format!("{}: {error}", temporary.display())))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))
}

/// Deliberate stop, so a replaced node withdraws its claim on the way out.
#[cfg(unix)]
mod stop {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// Take SIGTERM and SIGINT. A supervisor stopping the unit is the normal
    /// end of an ephemeral node, not an error path.
    pub fn install() -> Result<Arc<AtomicBool>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
            signal_hook::flag::register(signal, Arc::clone(&stop))
                .map_err(|error| format!("listen for signal {signal}: {error}"))?;
        }
        Ok(stop)
    }

    /// Sleep for `seconds`, returning false if a stop arrived first.
    ///
    /// Polled in short slices so a stop is noticed in a quarter second rather
    /// than after a whole renewal period — the supervisor's stop timeout is
    /// measured in seconds, and a drain that misses it is a SIGKILL.
    pub fn sleep(stop: &AtomicBool, seconds: u64) -> bool {
        let total = Duration::from_secs(seconds.max(1));
        let slice = Duration::from_millis(250);
        let mut slept = Duration::ZERO;
        while slept < total {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            let step = slice.min(total - slept);
            std::thread::sleep(step);
            slept += step;
        }
        !stop.load(Ordering::Relaxed)
    }
}

#[cfg(not(unix))]
mod stop {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    pub fn install() -> Result<Arc<AtomicBool>, String> {
        Ok(Arc::new(AtomicBool::new(false)))
    }

    pub fn sleep(_stop: &AtomicBool, seconds: u64) -> bool {
        std::thread::sleep(std::time::Duration::from_secs(seconds.max(1)));
        true
    }
}

fn one_path(args: &[String]) -> Result<PathBuf, Error> {
    match args {
        [path] => Ok(PathBuf::from(path)),
        _ => Err(Error::Invalid(usage())),
    }
}

fn pack_args(args: &[String]) -> Result<(PathBuf, Option<PathBuf>), Error> {
    match args {
        [root] => Ok((PathBuf::from(root), None)),
        [root, flag, output] if flag == "-o" || flag == "--output" => {
            Ok((PathBuf::from(root), Some(PathBuf::from(output))))
        }
        _ => Err(Error::Invalid(usage())),
    }
}

fn optional_arg(args: &[String], name: &str) -> Result<(PathBuf, Option<String>), Error> {
    let Some(first) = args.first() else {
        return Err(Error::Invalid(usage()));
    };
    let mut value = None;
    let mut index = 1;
    while index < args.len() {
        if args[index] == name {
            value = args.get(index + 1).cloned();
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok((PathBuf::from(first), value))
}

fn named_arg(args: &[String], name: &str) -> Option<PathBuf> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
}

fn named_text(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn signature_path(package: &Path) -> PathBuf {
    let mut path = package.to_path_buf();
    path.set_extension(format!(
        "{}.asc",
        package
            .extension()
            .map_or("spore", |e| e.to_str().unwrap_or("spore"))
    ));
    path
}

#[cfg(test)]
mod announce_tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|value| (*value).to_owned()).collect()
    }

    fn lease_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "soil-spore-announce-{}-{}/application-services.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ))
    }

    #[test]
    fn renewing_is_comfortably_faster_than_expiring() {
        assert_eq!(renewal(None, 240).expect("default"), 80);
        assert_eq!(renewal(Some(30), 240).expect("explicit"), 30);
    }

    #[test]
    fn a_renewal_that_outlives_the_lease_is_refused() {
        for every in [Some(240), Some(600), Some(0)] {
            let refused = renewal(every, 240);
            assert!(
                refused.is_err(),
                "{every:?} would leave the pool every cycle"
            );
        }
    }

    #[test]
    fn the_node_is_never_told_who_it_is() {
        // `--node-id` was how a hand-run test named a node. An unattended node
        // has nobody to pass it, and a provisioned value would be the same id
        // on every replica, so the option is gone rather than optional.
        let refused = Announce::parse(&args(&[
            "/spores/confluence",
            "--node-id",
            "2fd5bf85",
            "--endpoint",
            "http://10.0.0.6:8788",
        ]));
        assert!(matches!(refused, Err(Error::Invalid(_))));
    }

    #[test]
    fn the_lease_lands_in_the_home_it_read_the_identity_from() {
        let parsed = Announce::parse(&args(&[
            "/spores/confluence",
            "--home",
            "/var/lib/mycelium",
            "--endpoint",
            "http://10.0.0.6:8788",
        ]))
        .expect("options parse");
        assert_eq!(parsed.home, PathBuf::from("/var/lib/mycelium"));
        assert_eq!(
            parsed.out,
            PathBuf::from("/var/lib/mycelium/application-services.json")
        );
    }

    #[test]
    fn draining_takes_only_our_claim() {
        let path = lease_path();
        // Two Spores on one node: `myceliumd` reads a single document, so a
        // drain must remove one claim and leave the other serving.
        let ours = serde_json::json!({"observations": [
            {"service_id": "service/aaaa/application/confluence"},
            {"service_id": "service/bbbb/application/unibus"}
        ]});
        publish(&path, &ours).expect("publish both");

        assert!(withdraw(&path, "service/aaaa/application/confluence").expect("withdraw"));
        let left = observations(&path);
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0]["service_id"], "service/bbbb/application/unibus");
        assert!(
            !withdraw(&path, "service/aaaa/application/confluence").expect("second withdraw"),
            "draining twice is a no-op, not an error"
        );
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn a_loopback_lease_is_recognised() {
        for local in [
            "http://127.0.0.1:8788",
            "http://localhost:8788",
            "http://[::1]:8788",
        ] {
            assert!(
                loopback(local),
                "{local} cannot be dialled from another node"
            );
        }
        for remote in ["http://10.253.0.6:8788", "https://10.0.0.6:443"] {
            assert!(!loopback(remote), "{remote} is reachable");
        }
    }
}
