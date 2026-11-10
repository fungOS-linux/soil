//! `tend`: compose a Spore's Canvas module, then pack it.
//!
//! Two calls, one process: [`tend_core::compose`] turns the view, its components
//! and the SDK modules into one source, and [`soil_spore::pack`] turns the Spore
//! directory into a `.spore`. Nothing here shells out, which is the whole reason
//! this exists next to `soil-spore` rather than in a separate tool.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use soil_spore::{Manifest, pack, verify};
use tend_core::compose;

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tend: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("compose") => compose_command(&args[1..]),
        Some("pack") => pack_command(&args[1..], false),
        Some("build") => pack_command(&args[1..], true),
        Some("help" | "--help" | "-h") => {
            println!("{}", usage());
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n{}", usage())),
        None => Err(usage()),
    }
}

fn usage() -> String {
    "usage: tend compose <spore-dir> [--component FILE]… [-o out.lua]\n       \
     tend pack <spore-dir> [-o out.spore] [--json]\n       \
     tend build <spore-dir> [--component FILE]… [-o out.spore] [--json]"
        .into()
}

/// A Spore's Canvas module: its declared entry plus each component, in order.
fn composed(spore: &Path, components: &[PathBuf]) -> Result<String, String> {
    let manifest = Manifest::load(spore).map_err(|error| error.to_string())?;
    let entry = spore.join(entry_of(&manifest)?);
    let mut parts = vec![read(&entry)?];
    for component in components {
        parts.push(read(component)?);
    }
    compose(&parts).map_err(|error| error.to_string())
}

fn compose_command(args: &[String]) -> Result<(), String> {
    let (spore, components, out, _) = parse_args(args)?;
    let module = composed(&spore, &components)?;
    match out {
        Some(out) => {
            write(&out, module.as_bytes())?;
            println!("{}", out.display());
        }
        None => println!("{module}"),
    }
    Ok(())
}

/// Pack the Spore, composing first when this is a build.
///
/// A build stages a copy rather than writing into the source tree: the composed
/// entry has to be what the package contains, and mutating somebody's checkout
/// to achieve that is not a build tool's job.
fn pack_command(args: &[String], build: bool) -> Result<(), String> {
    let (spore, components, out, json) = parse_args(args)?;
    let staged = if build {
        let module = composed(&spore, &components)?;
        Some(stage(&spore, &module)?)
    } else {
        None
    };
    let root = staged.as_deref().unwrap_or(&spore);
    let bytes = pack(root).map_err(|error| error.to_string())?;
    let (manifest, lock) = verify(&bytes).map_err(|error| error.to_string())?;
    let path = out.unwrap_or_else(|| default_package(&spore, &manifest));
    write(&path, &bytes)?;
    if json {
        let anycast = manifest.anycast.as_ref().map(|claim| {
            serde_json::json!({
                "name": claim.name,
                "kind": claim.kind,
                "ttl_secs": claim.ttl_secs,
            })
        });
        println!(
            "{}",
            serde_json::json!({
                "path": path.display().to_string(),
                "bytes": bytes.len(),
                "digest": lock.digest,
                "spore": manifest.spore.id,
                "version": manifest.spore.version,
                "anycast": anycast,
            })
        );
    } else {
        println!("{} {} bytes {}", path.display(), bytes.len(), lock.digest);
    }
    if let Some(staged) = staged {
        let _ = std::fs::remove_dir_all(staged);
    }
    Ok(())
}

/// The Spore's declared Canvas entry. A Spore without one draws no chrome and
/// has nothing for `tend` to compose, which is a refusal rather than an empty
/// module that installs and shows nothing.
fn entry_of(manifest: &Manifest) -> Result<&str, String> {
    manifest
        .ui
        .as_ref()
        .map(|ui| ui.entry.as_str())
        .ok_or_else(|| "this Spore declares no [ui] entry, so there is nothing to compose".into())
}

fn default_package(spore: &Path, manifest: &Manifest) -> PathBuf {
    let name = spore
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "spore".into());
    PathBuf::from(format!("{name}-{}.spore", manifest.spore.version))
}

/// Copy the Spore to a scratch directory with its entry replaced by the
/// composed module, so the package contains what was built.
fn stage(spore: &Path, module: &str) -> Result<PathBuf, String> {
    let manifest = Manifest::load(spore).map_err(|error| error.to_string())?;
    let entry = entry_of(&manifest)?.to_owned();
    let staged = std::env::temp_dir().join(format!(
        "tend-build-{}-{}",
        std::process::id(),
        spore
            .file_name()
            .map_or("spore", |name| { name.to_str().unwrap_or("spore") })
    ));
    let _ = std::fs::remove_dir_all(&staged);
    copy_tree(spore, &staged)?;
    write(&staged.join(&entry), module.as_bytes())?;
    // A copied lock describes the sources, not the composed module.
    let _ = std::fs::remove_file(staged.join(soil_spore::LOCK_NAME));
    Ok(staged)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))? {
        let entry = entry.map_err(|error| error.to_string())?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &target)?;
        } else if source.is_file() {
            std::fs::copy(&source, &target)
                .map_err(|error| format!("{}: {error}", source.display()))?;
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

fn parse_args(args: &[String]) -> Result<(PathBuf, Vec<PathBuf>, Option<PathBuf>, bool), String> {
    let Some(spore) = args.first().map(PathBuf::from) else {
        return Err(usage());
    };
    let mut components = Vec::new();
    let mut out = None;
    let mut json = false;
    let mut index = 1;
    while index < args.len() {
        let value = |index: usize| {
            args.get(index)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", args[index - 1]))
        };
        match args[index].as_str() {
            "--component" => {
                components.push(PathBuf::from(value(index + 1)?));
                index += 2;
            }
            "-o" | "--out" => {
                out = Some(PathBuf::from(value(index + 1)?));
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => return Err(format!("unknown option `{other}`\n{}", usage())),
        }
    }
    Ok((spore, components, out, json))
}
