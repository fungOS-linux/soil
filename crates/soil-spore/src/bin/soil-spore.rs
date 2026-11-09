//! `soil-spore`: build and verify Spore packages.
//!
//! Soil owns make and verify. Publishing is a tar over IPFS; trust roots come
//! from Mycelium (a bound PGP key) or an external keyring.

use soil_spore::manual::{Manual, read_prose, render_info};
use soil_spore::{
    Error, Manifest, check, pack, publish_blockers, read_package, sign, verify, verify_signature,
    write_lock,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

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
    "usage: soil-spore check <dir> [--publish]\n       soil-spore lock <dir>\n       soil-spore digest <dir>\n       soil-spore man <dir|package.spore> [--width N]\n       soil-spore info <dir|package.spore>\n       soil-spore pack <dir> [-o out.spore]\n       soil-spore verify <package.spore>\n       soil-spore sign <package.spore> [--key ID]\n       soil-spore verify-signature <package.spore> [--keyring FILE] [--key ID]\n       soil-spore --version\n".into()
}

/// Read a Spore from a directory or a packed package.
fn load(path: &Path) -> Result<(Manifest, Option<BTreeMap<String, Vec<u8>>>), Error> {
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
