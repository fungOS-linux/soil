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
use soil_store::publish::{self, DelegationRequest, SignRequest};
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
    if matches!(command, "listing") {
        return build_listing(rest);
    }
    if matches!(command, "search") {
        return search(rest);
    }
    if matches!(command, "review") {
        return review(rest);
    }
    if matches!(command, "publish") {
        return publish_all(rest);
    }
    if matches!(command, "add") {
        return add_media(rest);
    }
    if matches!(command, "delegate") {
        return delegate(rest);
    }
    if matches!(command, "sign") {
        return sign_index(rest);
    }
    if matches!(command, "revoke") {
        return revoke(rest);
    }
    if matches!(command, "verify") {
        let index_path = named(rest, "--index").ok_or_else(|| Error::Invalid(usage()))?;
        let operator = named(rest, "--operator-key").ok_or_else(|| Error::Invalid(usage()))?;
        let index = Index::read(Path::new(&index_path))?;
        let key =
            std::fs::read(&operator).map_err(|error| Error::Io(format!("{operator}: {error}")))?;
        let verified = index.verify(&key)?;
        println!(
            "ok {} entries {} delegations",
            index.entries.len(),
            verified.delegations.len()
        );
        return Ok(());
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
            let mut store = Store::new(Some(&index), installed, source(rest, &index));
            if let Some(packages) = named(rest, "--packages") {
                store = store.with_packages(packages);
            }
            if let Some(media) = named(rest, "--media") {
                store = store.with_media(media);
            }
            if let Some(reviews) = named(rest, "--reviews") {
                store = store.with_reviews(soil_store::Reviews::read(Path::new(&reviews))?);
            }
            if let Some(page) = named(rest, "--page") {
                store = store.with_page(&page);
            }
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
        verified: !index.checkpoint.signature.trim().is_empty()
            || !index.delegations.is_empty()
            || index
                .entries
                .iter()
                .any(|entry| !entry.signature.trim().is_empty()),
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
    // so the index references bytes rather than a path on this machine. Packing
    // through the existing lock keeps the publisher's tree untouched: a store
    // that refreshed locks as it scanned would be editing what it reviewed.
    let package = soil_store::cid::raw(&soil_spore::pack_locked(dir)?);
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
        devices: {
            let mut devices = manifest
                .placement
                .as_ref()
                .map(|placement| placement.devices.clone())
                .unwrap_or_default();
            devices.sort();
            devices
        },
        publisher: None,
        publisher_key: None,
        signature: String::new(),
    })
}

/// `publish`: pack, sign, index and countersign a whole shelf in one pass.
///
/// The per-package commands are the primitive; this is the loop a publisher
/// actually runs, and it reports what it skipped instead of stopping at the
/// first tree that is not ready. Nothing is written until every package that
/// made it into the index has its tar and its signature.
fn publish_all(rest: &[String]) -> Result<()> {
    let root = named(rest, "--spores-root").ok_or_else(|| Error::Invalid(usage()))?;
    let packages_dir = named(rest, "--packages").ok_or_else(|| Error::Invalid(usage()))?;
    let publisher = named(rest, "--publisher").ok_or_else(|| Error::Invalid(usage()))?;
    let key = named(rest, "--key").ok_or_else(|| Error::Invalid(usage()))?;
    let delegation_path = named(rest, "--delegation").ok_or_else(|| Error::Invalid(usage()))?;
    let operator = named(rest, "--operator-key");
    let operator_public = named(rest, "--operator-public");
    let home = named(rest, "--gnupg-home").map(PathBuf::from);
    let source_name = named(rest, "--source").unwrap_or_else(|| "fungos".into());

    let mut shelf = Vec::new();
    let mut skipped = Vec::new();
    for dir in packages(Path::new(&root))? {
        match entry_for(&dir) {
            Ok(entry) => shelf.push((dir, entry)),
            Err(error) => skipped.push(format!("{error}")),
        }
    }
    shelf.sort_by(|a, b| a.1.spore_id.cmp(&b.1.spore_id));
    let mut index = Index::seal(
        &source_name,
        shelf.iter().map(|(_, entry)| entry.clone()).collect(),
    );

    // Tars first: an index that names a package nobody can fetch is a promise
    // the store cannot keep.
    let packages_path = Path::new(&packages_dir);
    std::fs::create_dir_all(packages_path)
        .map_err(|error| Error::Io(format!("{packages_dir}: {error}")))?;
    let mut packed = 0;
    for (dir, entry) in &shelf {
        let bytes = soil_spore::pack_locked(dir)?;
        let tar = packages_path.join(format!("{}.spore", entry.digest));
        let signature = packages_path.join(format!("{}.spore.asc", entry.digest));
        if !tar.is_file() {
            std::fs::write(&tar, &bytes)
                .map_err(|error| Error::Io(format!("{}: {error}", tar.display())))?;
            packed += 1;
        }
        if !signature.is_file() {
            // Sign the bytes we just wrote, through the same GnuPG home the
            // entries are signed in, so one `--gnupg-home` covers the package.
            let detached = soil_spore::sign_detached(home.as_deref(), &key, &bytes)?;
            std::fs::write(&signature, detached)
                .map_err(|error| Error::Io(format!("{}: {error}", signature.display())))?;
        }
    }

    let source = std::fs::read_to_string(&delegation_path)
        .map_err(|error| Error::Io(format!("{delegation_path}: {error}")))?;
    let delegation: soil_store::Delegation = serde_json::from_str(&source)
        .map_err(|error| Error::Invalid(format!("{delegation_path}: {error}")))?;
    publish::merge_delegations(&mut index, vec![delegation]);

    let signed = publish::sign(
        &mut index,
        SignRequest {
            publisher: &publisher,
            key: &key,
            home: home.as_deref(),
            countersign: operator.as_deref(),
        },
    )?;
    index.validate()?;
    if let Some(public) = &operator_public {
        let bytes =
            std::fs::read(public).map_err(|error| Error::Io(format!("{public}: {error}")))?;
        index.verify(&bytes)?;
    }

    eprintln!(
        "soil-store: {} entries, {packed} packed, {} skipped",
        signed,
        skipped.len()
    );
    for line in &skipped {
        eprintln!("  skipped {line}");
    }
    match named(rest, "--index") {
        Some(path) => {
            if !flag(rest, "--write") {
                return emit(rest, &index);
            }
            let json = serde_json::to_string_pretty(&index)
                .map_err(|error| Error::Json(error.to_string()))?;
            let temporary = Path::new(&path).with_extension("json.tmp");
            std::fs::write(&temporary, format!("{json}\n"))
                .map_err(|error| Error::Io(format!("{}: {error}", temporary.display())))?;
            std::fs::rename(&temporary, &path)
                .map_err(|error| Error::Io(format!("{path}: {error}")))
        }
        None => emit(rest, &index),
    }
}

/// `review`: the operator's review file.
///
/// The store holds the claims; it does not invent them. `add` appends what a
/// signed-in principal said about one digest, `hide` records why a review stops
/// counting, and `list` is what the operator reads before deciding.
fn review(rest: &[String]) -> Result<()> {
    let path = named(rest, "--reviews").ok_or_else(|| Error::Invalid(usage()))?;
    let mut reviews = soil_store::Reviews::read(Path::new(&path))?;
    match positional(rest).as_deref() {
        Some("add") => {
            let rating = named(rest, "--rating")
                .ok_or_else(|| Error::Invalid(usage()))?
                .parse::<u8>()
                .map_err(|_| Error::Invalid("--rating must be 1..=5".into()))?;
            reviews.add(soil_store::review::Review {
                spore_id: named(rest, "--spore").ok_or_else(|| Error::Invalid(usage()))?,
                version: named(rest, "--version").ok_or_else(|| Error::Invalid(usage()))?,
                digest: named(rest, "--digest").ok_or_else(|| Error::Invalid(usage()))?,
                principal: named(rest, "--principal").ok_or_else(|| Error::Invalid(usage()))?,
                rating,
                body: named(rest, "--body").unwrap_or_default(),
                created_at: now_unix_s(),
            })?;
        }
        Some("hide") => reviews.hide(soil_store::review::Hidden {
            digest: named(rest, "--digest").ok_or_else(|| Error::Invalid(usage()))?,
            principal: named(rest, "--principal").ok_or_else(|| Error::Invalid(usage()))?,
            reason: named(rest, "--reason").ok_or_else(|| Error::Invalid(usage()))?,
            hidden_at: now_unix_s(),
        })?,
        Some("list") => {
            let digest = named(rest, "--digest");
            let rows: Vec<_> = match &digest {
                Some(digest) => reviews.visible(digest),
                None => reviews.reviews.iter().collect(),
            };
            if flag(rest, "--json") {
                println!(
                    "{}",
                    serde_json::json!({ "total": rows.len(), "reviews": rows })
                );
            } else {
                for row in rows {
                    println!(
                        "{}★ {:<24} {} {}  {}",
                        row.rating, row.principal, row.spore_id, row.version, row.body
                    );
                }
            }
            return Ok(());
        }
        _ => return Err(Error::Invalid(usage())),
    }
    if flag(rest, "--write") {
        reviews.write(Path::new(&path))?;
        eprintln!("soil-store: {} reviews → {path}", reviews.reviews.len());
        return Ok(());
    }
    emit(rest, &reviews)
}

fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// `search`: the same answer the store's `/api/search` gives.
///
/// A CLI and a page that rank differently are two stores; the query lives in one
/// function, and this is a thin way to call it.
fn search(rest: &[String]) -> Result<()> {
    let index_path = named(rest, "--index").ok_or_else(|| Error::Invalid(usage()))?;
    let spores_root = named(rest, "--spores-root").ok_or_else(|| Error::Invalid(usage()))?;
    let index = Index::read(Path::new(&index_path))?;
    let installed = scan(Path::new(&spores_root), &pairs(rest, "--pin")?)?;
    let mut store = Store::new(Some(&index), installed, source(rest, &index));
    if let Some(packages) = named(rest, "--packages") {
        store = store.with_packages(packages);
    }
    if let Some(media) = named(rest, "--media") {
        store = store.with_media(media);
    }
    let query = positional(rest).unwrap_or_default();
    let category = named(rest, "--category");
    let rows: Vec<_> = store
        .search(&query, category.as_deref())
        .into_iter()
        .map(|entry| store.row(entry))
        .collect();
    if flag(rest, "--json") {
        println!(
            "{}",
            serde_json::json!({ "query": query, "category": category, "total": rows.len(), "entries": rows })
        );
        return Ok(());
    }
    for row in &rows {
        println!(
            "{:<32} {:<8} {}",
            row["id"].as_str().unwrap_or_default(),
            row["version"].as_str().unwrap_or_default(),
            row["document"]["summary"]
                .as_str()
                .or_else(|| row["listing"].as_str())
                .unwrap_or("")
        );
    }
    eprintln!("{} match(es)", rows.len());
    Ok(())
}

/// `listing`: the document a storefront and an agent both read.
///
/// The bytes are addressed, so the same listing is one CID: write it into the
/// media directory and a publisher pastes that CID into `[catalog].record`.
fn build_listing(rest: &[String]) -> Result<()> {
    let root = positional(rest).ok_or_else(|| Error::Invalid(usage()))?;
    let listing = soil_store::Listing::build(Path::new(&root))?;
    match named(rest, "--media") {
        Some(dir) => {
            let bytes = serde_json::to_vec_pretty(&listing)
                .map_err(|error| Error::Json(error.to_string()))?;
            let address = soil_store::media::add(Path::new(&dir), &bytes)?;
            println!("{address}");
        }
        None => emit(rest, &listing)?,
    }
    Ok(())
}

/// `add`: put files in the content-addressed directory and print their CIDs.
///
/// This is how an icon or a screenshot becomes an `ipfs://` link the manifest can
/// carry: the bytes are the address, so nothing is renamed or re-encoded.
fn add_media(rest: &[String]) -> Result<()> {
    let dir = named(rest, "--media").ok_or_else(|| Error::Invalid(usage()))?;
    let files = positionals(rest);
    if files.is_empty() {
        return Err(Error::Invalid(usage()));
    }
    for file in files {
        let address = soil_store::media::add_file(Path::new(&dir), Path::new(&file))?;
        println!("{address}\t{file}");
    }
    Ok(())
}

/// `delegate`: the operator authorizes one publisher key.
///
/// The delegation is the only thing the operator signs for a publisher, so this
/// is the whole start-of-trust path: no key copy-pasting, no hand-built payload.
fn delegate(rest: &[String]) -> Result<()> {
    let publisher = named(rest, "--publisher").ok_or_else(|| Error::Invalid(usage()))?;
    let operator_key = named(rest, "--operator-key").ok_or_else(|| Error::Invalid(usage()))?;
    let public_key = named(rest, "--public-key").map(PathBuf::from);
    let fingerprint = named(rest, "--fingerprint");
    let home = named(rest, "--gnupg-home").map(PathBuf::from);
    let scope = named(rest, "--scope").unwrap_or_else(|| "spore".into());
    let days = match named(rest, "--days") {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| Error::Invalid("--days must be a whole number".into()))?,
        None => 0,
    };
    let delegation = publish::delegate(DelegationRequest {
        publisher: &publisher,
        operator_key: &operator_key,
        home: home.as_deref(),
        scope: &scope,
        days,
        public_key: public_key.as_deref(),
        fingerprint: fingerprint.as_deref(),
    })?;
    emit(rest, &delegation)
}

/// `sign`: bind entries to a publisher and reseal the checkpoint.
fn sign_index(rest: &[String]) -> Result<()> {
    let path = named(rest, "--index").ok_or_else(|| Error::Invalid(usage()))?;
    let publisher = named(rest, "--publisher").ok_or_else(|| Error::Invalid(usage()))?;
    let key = named(rest, "--key").ok_or_else(|| Error::Invalid(usage()))?;
    let home = named(rest, "--gnupg-home").map(PathBuf::from);
    let countersign = named(rest, "--countersign");
    let mut index = Index::read(Path::new(&path))?;
    let mut delegations = Vec::new();
    for file in repeated(rest, "--delegation") {
        let source = std::fs::read_to_string(&file)
            .map_err(|error| Error::Io(format!("{file}: {error}")))?;
        let delegation: soil_store::Delegation = serde_json::from_str(&source)
            .map_err(|error| Error::Invalid(format!("{file}: {error}")))?;
        delegations.push(delegation);
    }
    if !delegations.is_empty() {
        publish::merge_delegations(&mut index, delegations);
    }
    let signed = publish::sign(
        &mut index,
        SignRequest {
            publisher: &publisher,
            key: &key,
            home: home.as_deref(),
            countersign: countersign.as_deref(),
        },
    )?;
    index.validate()?;
    eprintln!("soil-store: signed {signed} entries for {publisher}");
    emit_or_write(rest, &path, &index)
}

/// `revoke`: withdraw one delegated key without rewriting the delegation.
fn revoke(rest: &[String]) -> Result<()> {
    let path = named(rest, "--index").ok_or_else(|| Error::Invalid(usage()))?;
    let publisher = named(rest, "--publisher").ok_or_else(|| Error::Invalid(usage()))?;
    let key = named(rest, "--key").ok_or_else(|| Error::Invalid(usage()))?;
    let reason = named(rest, "--reason").ok_or_else(|| Error::Invalid(usage()))?;
    let operator_key = named(rest, "--operator-key").ok_or_else(|| Error::Invalid(usage()))?;
    let home = named(rest, "--gnupg-home").map(PathBuf::from);
    let mut index = Index::read(Path::new(&path))?;
    let revocation = publish::revoke(
        &mut index,
        &publisher,
        &key,
        &reason,
        &operator_key,
        home.as_deref(),
    )?;
    index.validate()?;
    eprintln!(
        "soil-store: revoked {} key {}",
        revocation.publisher, revocation.fingerprint
    );
    emit_or_write(rest, &path, &index)
}

/// Print a value as JSON, or write it to `-o`.
fn emit<T: serde::Serialize>(rest: &[String], value: &T) -> Result<()> {
    let json =
        serde_json::to_string_pretty(value).map_err(|error| Error::Json(error.to_string()))?;
    match named(rest, "-o") {
        Some(path) => std::fs::write(&path, format!("{json}\n"))
            .map_err(|error| Error::Io(format!("{path}: {error}"))),
        None => {
            println!("{json}");
            Ok(())
        }
    }
}

/// Replace the index in place with `--write`, otherwise print it.
///
/// Every write goes through a temporary file in the same directory, so a killed
/// process leaves the previous index rather than half of the next one.
fn emit_or_write(rest: &[String], path: &str, index: &Index) -> Result<()> {
    if !flag(rest, "--write") {
        return emit(rest, index);
    }
    let json =
        serde_json::to_string_pretty(index).map_err(|error| Error::Json(error.to_string()))?;
    let target = Path::new(path);
    let temporary = target.with_extension("json.tmp");
    std::fs::write(&temporary, format!("{json}\n"))
        .map_err(|error| Error::Io(format!("{}: {error}", temporary.display())))?;
    std::fs::rename(&temporary, target).map_err(|error| Error::Io(format!("{path}: {error}")))?;
    Ok(())
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
    "usage: soil-store index    --spores-root DIR [--source NAME] [-o FILE]
       soil-store listing  DIR [--media DIR] [-o FILE]
       soil-store search   QUERY --index FILE --spores-root DIR [--media DIR] [--packages DIR] [--category NAME] [--json]
       soil-store review   add|hide|list --reviews FILE [--spore ID] [--version V] [--digest D] [--principal P]
                           [--rating 1-5] [--body TEXT] [--reason TEXT] [--write] [--json]
       soil-store publish  --spores-root DIR --packages DIR --publisher PRINCIPAL --key KEY
                           --delegation FILE [--operator-key KEY] [--operator-public FILE]
                           [--source NAME] [--gnupg-home DIR] [--index FILE] [--write]
       soil-store add      --media DIR FILE...
       soil-store delegate --publisher PRINCIPAL --operator-key KEY (--public-key FILE | --fingerprint KEY)
                           [--scope spore|*] [--days N] [--gnupg-home DIR] [-o FILE]
       soil-store sign     --index FILE --publisher PRINCIPAL --key KEY [--delegation FILE]...
                           [--countersign OPERATOR-KEY] [--gnupg-home DIR] [--write]
       soil-store revoke   --index FILE --publisher PRINCIPAL --key KEY --reason TEXT
                           --operator-key KEY [--gnupg-home DIR] [--write]
       soil-store verify   --index FILE --operator-key FILE
       soil-store plan     --index FILE --spores-root DIR [--json] [--pin ID=LABEL]
       soil-store snapshot --index FILE --spores-root DIR [-o FILE] [--source-name NAME] [--source-index CID]
       soil-store serve    --index FILE --spores-root DIR [--packages DIR] [--media DIR] [--page DIR] [--reviews FILE] [--addr HOST:PORT] [--pin ID=LABEL] [--source-name NAME]
       soil-store --version"
        .into()
}

/// Every value given after a repeatable flag, in order.
fn repeated(args: &[String], flag: &str) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(index, arg)| arg.as_str() == flag && args.get(index + 1).is_some())
        .filter_map(|(index, _)| args.get(index + 1).cloned())
        .collect()
}

fn named(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

/// Every argument that is a value rather than a flag or a flag's value.
///
/// The command line here is small and every flag that takes a value takes
/// exactly one, so "the token after a `-`-prefixed token is its value" is the
/// whole rule; boolean flags are only used by commands that take no positionals.
fn positionals(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg.starts_with('-') {
            skip = true;
            continue;
        }
        out.push(arg.clone());
    }
    out
}

fn positional(args: &[String]) -> Option<String> {
    positionals(args).into_iter().next()
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
