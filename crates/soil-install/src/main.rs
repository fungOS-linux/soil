//! `soil-install`: host-side Spore install, apply, and rollback.
//!
//! This is the authority the storefront asks for. It never fetches by itself;
//! a package tar must already be local, and `apply` requires a signed index and
//! a publisher keyring unless `--allow-unsigned-dev` is explicit.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use soil_install::{
    adopt, install_cid, sync, SyncOptions,ApplyOptions, InstallOptions, apply, install, rollback, status};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-install: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage().into());
    };
    if matches!(command, "--help" | "-h" | "help") {
        println!("{}", usage());
        return Ok(());
    }
    if matches!(command, "--version" | "-V" | "version") {
        println!("soil-install {VERSION}");
        return Ok(());
    }
    let rest = &args[1..];
    match command {
        "install" => {
            let root = PathBuf::from(required(rest, "--root")?);
            // A CID is the package's address; the index is what knows which
            // digest and publisher key it belongs to.
            if let Some(cid) = named(rest, "--cid") {
                let index = PathBuf::from(required(rest, "--index")?);
                let packages = PathBuf::from(required(rest, "--packages")?);
                let state = install_cid(cid, &index, &packages, named(rest, "--origin"), &root)
                    .map_err(|error| error.to_string())?;
                if flag(rest, "--json") {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&state).map_err(|error| error.to_string())?
                    );
                } else {
                    println!(
                        "installed {} {} ({}) previous={}",
                        state.id,
                        state.version,
                        state.digest,
                        state.previous_digest.as_deref().unwrap_or("none")
                    );
                }
                return Ok(());
            }
            let package = first_path(rest)?;
            let keyring = named(rest, "--keyring").map(PathBuf::from);
            let state = install(
                &package,
                &root,
                InstallOptions {
                    keyring: keyring.as_deref(),
                    key: named(rest, "--key"),
                    publisher: named(rest, "--publisher"),
                    public_key: None,
                },
            )
            .map_err(|error| error.to_string())?;
            if flag(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&state).map_err(|error| error.to_string())?
                );
            } else {
                println!(
                    "installed {} {} ({}) previous={}",
                    state.id,
                    state.version,
                    state.digest,
                    state.previous_digest.as_deref().unwrap_or("none")
                );
            }
            Ok(())
        }
        "rollback" => {
            let id = required(rest, "--id")?;
            let root = PathBuf::from(required(rest, "--root")?);
            let state = rollback(id, &root).map_err(|error| error.to_string())?;
            if flag(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&state).map_err(|error| error.to_string())?
                );
            } else {
                println!(
                    "rolled back {} to {} ({})",
                    state.id, state.version, state.digest
                );
            }
            Ok(())
        }
        "status" => {
            let root = PathBuf::from(required(rest, "--root")?);
            let states = status(&root).map_err(|error| error.to_string())?;
            if flag(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&states).map_err(|error| error.to_string())?
                );
            } else if states.is_empty() {
                println!("no installed Spores");
            } else {
                for state in states {
                    println!(
                        "{:<32} {}  {}  previous={}",
                        state.id,
                        state.version,
                        state.digest,
                        state.previous_digest.as_deref().unwrap_or("none")
                    );
                }
            }
            Ok(())
        }
        "apply" => {
            let index = PathBuf::from(required(rest, "--index")?);
            let root = PathBuf::from(required(rest, "--root")?);
            let packages = PathBuf::from(required(rest, "--packages")?);
            let operator_key = PathBuf::from(required(rest, "--operator-key")?);
            let outcomes = apply(
                &root,
                ApplyOptions {
                    index: &index,
                    packages: &packages,
                    operator_key: &operator_key,
                    write: flag(rest, "--write"),
                    allow_unsigned_dev: flag(rest, "--allow-unsigned-dev"),
                },
            )
            .map_err(|error| error.to_string())?;
            if flag(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&outcomes).map_err(|error| error.to_string())?
                );
            } else {
                for outcome in outcomes {
                    println!(
                        "{:<32} {}  {} -> {}  {}",
                        outcome.id, outcome.class, outcome.from, outcome.to, outcome.action
                    );
                }
            }
            Ok(())
        }
        "adopt" => {
            let root = PathBuf::from(required(rest, "--root")?);
            let adopted =
                adopt(&root, flag(rest, "--force")).map_err(|error| error.to_string())?;
            if flag(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&adopted).map_err(|error| error.to_string())?
                );
            } else if adopted.is_empty() {
                println!("nothing to adopt: every tree already has provenance");
            } else {
                for state in adopted {
                    if state.digest.is_empty() {
                        println!("{:<32} {}", state.id, state.action);
                    } else {
                        println!("{:<32} {}  {}", state.id, state.version, state.digest);
                    }
                }
            }
            Ok(())
        }
        "sync" => {
            let seed = PathBuf::from(required(rest, "--seed")?);
            let root = PathBuf::from(required(rest, "--root")?);
            let packages = named(rest, "--packages").map(PathBuf::from);
            let outcomes = sync(
                &root,
                SyncOptions {
                    seed: &seed,
                    packages: packages.as_deref(),
                    root: &root,
                    write: flag(rest, "--write"),
                },
            )
            .map_err(|error| error.to_string())?;
            if flag(rest, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&outcomes).map_err(|error| error.to_string())?
                );
            } else if outcomes.is_empty() {
                println!("the seed names no roles");
            } else {
                for outcome in outcomes {
                    println!(
                        "{:<32} {:<10} {} -> {}  {}",
                        outcome.id, outcome.role, outcome.from.as_deref().unwrap_or("absent"),
                        outcome.to, outcome.action
                    );
                }
            }
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{}", usage())),
    }
}

fn usage() -> &'static str {
    "soil-install install <package.spore> --root DIR [--keyring FILE] [--key FP] [--publisher P] [--json]\n\
     soil-install install --cid ipfs://CID --index FILE --packages DIR [--origin URL] --root DIR [--json]\n\
     soil-install adopt --root DIR [--force] [--json]\n\
     soil-install sync --seed DIR --root DIR [--packages DIR] [--write] [--json]\n\
     soil-install rollback --id ID --root DIR [--json]\n\
     soil-install status --root DIR [--json]\n\
     soil-install apply --index FILE --root DIR --packages DIR --operator-key FILE [--write] [--allow-unsigned-dev] [--json]\n\
     soil-install --version"
}

fn named<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn required<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    named(args, name).ok_or_else(|| format!("{name} is required\n\n{}", usage()))
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|argument| argument == name)
}

fn first_path(args: &[String]) -> Result<PathBuf, String> {
    let path = args
        .first()
        .filter(|argument| !argument.starts_with("--"))
        .ok_or_else(|| format!("a package path is required\n\n{}", usage()))?;
    Ok(Path::new(path).to_path_buf())
}
