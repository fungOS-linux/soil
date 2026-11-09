//! `soil-spore`: build and verify Spore packages.
//!
//! Soil owns make and verify. Publishing is a tar over IPFS; trust roots come
//! from Mycelium (a bound PGP key) or an external keyring.

use soil_spore::publisher::TrustedPublisher;
use soil_spore::{Error, Manifest, check, lease, pack, sign, verify, verify_signature, write_lock};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
        "check" => {
            let root = one_path(rest)?;
            let lock = check(&root)?;
            println!(
                "ok {} {} {} files {}",
                lock.id,
                lock.version,
                lock.files.len(),
                lock.digest
            );
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
            let signature = signature_path(&package);
            sign(&package, &signature, key.as_deref())?;
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
    "usage: soil-spore check|lock|digest <dir>\n       soil-spore pack <dir> [-o out.spore]\n       soil-spore verify <package.spore>\n       soil-spore sign <package.spore> [--key ID]\n       soil-spore verify-signature <package.spore> [--keyring FILE] [--key ID]\n       soil-spore announce <dir> --node-id ID --endpoint URI [--out FILE] [--every SECS] [--once] [--unsigned-dev]".into()
}

/// Publish this Spore's `[anycast]` claim as a Mycelium service lease.
///
/// The claim is not permission: a real announcement has to be authorised by the
/// package's publisher grant, and the trust view that decides that lives in
/// Mycelium. Until that adapter exists this refuses unless `--unsigned-dev` is
/// passed, so a local test cannot be mistaken for a fleet grant.
fn announce(args: &[String]) -> Result<(), Error> {
    let mut node_id = None;
    let mut endpoints: Vec<String> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut every = 120u64;
    let mut once = false;
    let mut unsigned = false;

    let Some(root) = args.first().map(PathBuf::from) else {
        return Err(Error::Invalid(usage()));
    };
    let mut index = 1;
    while index < args.len() {
        let value = |index: usize| {
            args.get(index)
                .cloned()
                .ok_or_else(|| Error::Invalid(format!("{} needs a value", args[index - 1])))
        };
        match args[index].as_str() {
            "--node-id" => {
                node_id = Some(value(index + 1)?);
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
                every = value(index + 1)?
                    .parse()
                    .map_err(|_| Error::Invalid("--every is seconds".into()))?;
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

    let node_id = node_id.ok_or_else(|| Error::Invalid("--node-id is required".into()))?;
    if endpoints.is_empty() {
        return Err(Error::Invalid("at least one --endpoint is required".into()));
    }
    let manifest = Manifest::load(&root)?;
    if manifest.anycast.is_none() {
        return Err(Error::Invalid(
            "this Spore declares no [anycast] claim, so it has no name to publish".into(),
        ));
    }
    if !unsigned {
        return Err(Error::Invalid(
            "refusing to announce without a publisher grant; Mycelium supplies the trust view. \
             Pass --unsigned-dev for a local test"
                .into(),
        ));
    }
    let lock = check(&root)?;
    let publisher = TrustedPublisher {
        principal: "unsigned-dev".into(),
        grant_id: "none".into(),
        key_id: "none".into(),
    };
    let out = out.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".mycelium/application-services.json")
    });

    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        let document = lease::document(&manifest, &lock, &publisher, &node_id, &endpoints, now)?;
        write_lease(&out, &document)?;
        println!(
            "announced {} -> {} (expires in {}s)",
            manifest.anycast.as_ref().map(|a| a.kind.as_str()).unwrap_or_default(),
            endpoints.join(","),
            manifest.anycast.as_ref().map(|a| a.ttl_secs).unwrap_or_default()
        );
        if once {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(every.max(1)));
    }
}

/// Replace our own observation in the lease file and leave everyone else's
/// alone: `myceliumd` reads one document, so two Spores on a node must merge
/// rather than clobber.
fn write_lease(path: &Path, document: &serde_json::Value) -> Result<(), Error> {
    let ours: Vec<serde_json::Value> = document["observations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let owned: Vec<String> = ours
        .iter()
        .filter_map(|observation| observation["service_id"].as_str().map(str::to_owned))
        .collect();

    let mut observations: Vec<serde_json::Value> = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|existing| existing["observations"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|observation| {
            observation["service_id"]
                .as_str()
                .is_none_or(|id| !owned.iter().any(|ours| ours == id))
        })
        .collect();
    observations.extend(ours);

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
