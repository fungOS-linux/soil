//! `soil-store`: build a catalog from a package tree, say what the catalog
//! would change, emit the table the storefront renders, and serve it on
//! loopback for the Mycelium gateway.
//!
//! This tool never fetches, verifies or installs anything: those are host
//! capabilities with consent in front, and keeping them out of this tool is
//! what lets it run on a reviewer's laptop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use soil_store::index::{Entry, EntryKind, Index};
use soil_store::plan::{Installed, plan};
use soil_store::serve::Store;
use soil_store::snapshot::{Source, build};
use soil_store::{Error, Result};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_ADDR: &str = "127.0.0.1:8800";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-store: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(Error::Invalid(usage()));
    };
    let rest = &args[1..];

    if matches!(command, "--version" | "-V" | "version") {
        println!("soil-store {VERSION}");
        return Ok(());
    }
    if matches!(command, "--help" | "-h" | "help") {
        println!("{}", usage());
        return Ok(());
    }

    if matches!(command, "index") {
        return build_index(rest);
    }

    let index_path = named(rest, "--index").ok_or_else(|| Error::Invalid(usage()))?;
    let spores_root = named(rest, "--spores-root").ok_or_else(|| Error::Invalid(usage()))?;
    let pins = pairs(rest, "--pin")?;

    let index = Index::read(Path::new(&index_path))?;
    let catalog = index.catalog();
    let installed = scan(Path::new(&spores_root), &pins)?;

    match command {
        "plan" => {
            let plan = plan(&installed, &catalog);
            if flag(rest, "--json") {
                let rows: Vec<_> = plan
                    .updates
                    .iter()
                    .map(|update| {
                        serde_json::json!({
                            "id": update.spore_id,
                            "from": update.from,
                            "to": update.to,
                            "class": update.class.as_str(),
                            "reason": update.reason,
                        })
                    })
                    .collect();
                let value = serde_json::json!({
                    "current": plan.current,
                    "yanked": plan.yanked,
                    "updates": rows,
                });
                println!("{value}");
            } else {
                for update in &plan.updates {
                    println!(
                        "{:<28} {} → {}  {:<8} {}",
                        update.spore_id,
                        update.from,
                        update.to,
                        update.class.as_str(),
                        update.reason
                    );
                }
                for id in &plan.yanked {
                    println!("{:<28} yanked by the active source", id);
                }
                println!(
                    "{} current, {} updates, {} yanked",
                    plan.current.len(),
                    plan.updates.len(),
                    plan.yanked.len()
                );
            }
        }
        "snapshot" => {
            let plan = plan(&installed, &catalog);
            let source = source(rest, &index);
            let snapshot = build(&installed, &plan, &catalog, &source);
            let lua = snapshot.to_lua();
            match named(rest, "-o") {
                Some(path) => std::fs::write(&path, lua)
                    .map_err(|error| Error::Io(format!("{path}: {error}")))?,
                None => print!("{lua}"),
            }
        }
        "serve" => {
            let addr = named(rest, "--addr").unwrap_or_else(|| DEFAULT_ADDR.to_owned());
            let store = Store::new(Some(&index), installed, source(rest, &index));
            let listener = soil_store::serve::bind(&addr)?;
            match listener.local_addr() {
                Ok(address) => eprintln!("soil-store: http://{address}/ on {addr}"),
                Err(_) => eprintln!("soil-store: http://{addr}/"),
            }
            eprintln!(
                "soil-store: {} installed, {} updates, checkpoint {}",
                store.installed.len(),
                store.plan.updates.len(),
                index.checkpoint.seq
            );
            soil_store::serve::serve(listener, store)?;
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

fn source(rest: &[String], index: &Index) -> Source {
    Source {
        name: named(rest, "--source-name").unwrap_or_else(|| index.source.clone()),
        index: named(rest, "--source-index")
            .unwrap_or_else(|| format!("ipfs://{}", index.checkpoint.root)),
        verified: !index.checkpoint.signature.trim().is_empty(),
    }
}

/// Build a catalog from the packages on disk.
///
/// A fresh catalog, not an append: `seq` is assigned in id order and the
/// checkpoint commits to the lot. Appending a release to an existing log is the
/// publisher's separate step; this is what makes the first one exist.
fn build_index(rest: &[String]) -> Result<()> {
    let root = named(rest, "--spores-root").ok_or_else(|| Error::Invalid(usage()))?;
    let source_name = named(rest, "--source").unwrap_or_else(|| "fungos".into());
    let mut entries = Vec::new();
    for dir in packages(Path::new(&root))? {
        match entry_for(&dir) {
            Ok(entry) => entries.push(entry),
            Err(error) => eprintln!("soil-store: skipping {}: {error}", dir.display()),
        }
    }
    entries.sort_by(|a, b| a.spore_id.cmp(&b.spore_id));
    let index = Index::seal(&source_name, entries);
    index.validate()?;
    let json =
        serde_json::to_string_pretty(&index).map_err(|error| Error::Json(error.to_string()))?;
    match named(rest, "-o") {
        Some(path) => {
            std::fs::write(&path, format!("{json}\n"))
                .map_err(|error| Error::Io(format!("{path}: {error}")))?;
            eprintln!(
                "soil-store: {} entries, checkpoint {} → {path}",
                index.entries.len(),
                index.checkpoint.seq
            );
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn entry_for(dir: &Path) -> Result<Entry> {
    let installed = Installed::read(dir, None, None)?;
    // The package address is the digest of the tar a gateway would hand back,
    // so the index references bytes rather than a path on this machine.
    let package = soil_store::cid::raw(&soil_spore::pack(dir)?);
    let manifest = installed.manifest;
    Ok(Entry {
        seq: 0,
        kind: EntryKind::Add,
        spore_id: manifest.spore.id.clone(),
        version: manifest.spore.version.clone(),
        digest: installed.digest,
        package,
        listing: manifest
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.record.clone()),
        module: installed.module,
        ui: installed.ui,
        grants: manifest.grants.keys().cloned().collect(),
        system: manifest.system.as_ref().map(|system| system.role.clone()),
        publisher: None,
        signature: String::new(),
    })
}

fn packages(root: &Path) -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(root)
        .map_err(|error| Error::Io(format!("{}: {error}", root.display())))?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir.join("spore.toml").is_file() {
            dirs.push(dir);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn scan(root: &Path, pins: &BTreeMap<String, String>) -> Result<Vec<Installed>> {
    let mut installed = Vec::new();
    for dir in packages(root)? {
        let id = soil_spore::Manifest::load(&dir)
            .map(|manifest| manifest.spore.id)
            .map_err(|error| Error::Invalid(format!("{}: {error}", dir.display())))?;
        // Display metadata may come from the catalog, but *trust* provenance is
        // what the host recorded at install time. Until the store records it,
        // pass `None`: using the catalog's current publisher as "what was
        // installed" would make a publisher change invisible by construction.
        match Installed::read(&dir, None, pins.get(&id).cloned()) {
            Ok(app) => installed.push(app),
            // A directory that names a file it does not carry is not an
            // installed package. Say so and keep scanning: one broken tree
            // should not hide every other Spore on the machine.
            Err(error) => eprintln!("soil-store: skipping {}: {error}", dir.display()),
        }
    }
    installed.sort_by(|a, b| a.id().cmp(b.id()));
    Ok(installed)
}

fn usage() -> String {
    "usage: soil-store index    --spores-root DIR [--source NAME] [-o FILE]\n       soil-store plan     --index FILE --spores-root DIR [--json] [--pin ID=LABEL]\n       soil-store snapshot --index FILE --spores-root DIR [-o FILE] [--source-name NAME] [--source-index CID]\n       soil-store serve    --index FILE --spores-root DIR [--addr HOST:PORT] [--pin ID=LABEL] [--source-name NAME]\n       soil-store --version".into()
}

fn named(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn pairs(args: &[String], flag: &str) -> Result<BTreeMap<String, String>> {
    let mut pairs = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == flag {
            let Some(value) = args.get(index + 1) else {
                return Err(Error::Invalid(format!("{flag} needs ID=LABEL")));
            };
            let Some((id, label)) = value.split_once('=') else {
                return Err(Error::Invalid(format!("{flag} needs ID=LABEL")));
            };
            pairs.insert(id.to_owned(), label.to_owned());
            index += 2;
            continue;
        }
        index += 1;
    }
    Ok(pairs)
}
