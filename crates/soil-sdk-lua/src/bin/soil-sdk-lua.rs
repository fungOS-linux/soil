//! Generate the Lua SDK from a WIT directory.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-sdk-lua: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    // Both defaults are repo-root relative, because that is where `--check` runs
    // in CI. `--wit wit` already assumes it, so the output follows the same rule
    // rather than resolving against whatever directory the caller started in.
    let mut wit = PathBuf::from("wit");
    let mut out = PathBuf::from("crates/soil-sdk-lua/generated");
    let mut package = None;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--wit" => wit = PathBuf::from(next(&mut args, "--wit")?),
            "--out" => out = PathBuf::from(next(&mut args, "--out")?),
            "--package" => package = Some(next(&mut args, "--package")?),
            "--check" => check = true,
            "--help" | "-h" => {
                println!(
                    "usage: soil-sdk-lua [--wit DIR] [--out DIR] [--package NAME] [--check]\n\
                     defaults: --wit wit --out crates/soil-sdk-lua/generated (repo root)"
                );
                return Ok(());
            }
            other => bail!("unknown argument {other}"),
        }
    }

    let generated = soil_sdk_lua::generate(&wit, package.as_deref())?;
    if check {
        check_file(&out.join("soil.lua"), &generated.lua)?;
        check_file(&out.join("soil-sdk.json"), &generated.manifest)?;
        println!("soil-sdk-lua: generated SDK is current");
        return Ok(());
    }

    std::fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;
    std::fs::write(out.join("soil.lua"), generated.lua)
        .with_context(|| format!("write {}", out.join("soil.lua").display()))?;
    std::fs::write(out.join("soil-sdk.json"), generated.manifest)
        .with_context(|| format!("write {}", out.join("soil-sdk.json").display()))?;
    println!("soil-sdk-lua: wrote {}", out.display());
    Ok(())
}

fn check_file(path: &std::path::Path, expected: &str) -> Result<()> {
    let actual = std::fs::read_to_string(path)
        .with_context(|| format!("read {}; regenerate with soil-sdk-lua", path.display()))?;
    if actual != expected {
        bail!("{} is stale; regenerate with soil-sdk-lua", path.display());
    }
    Ok(())
}

fn next(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .with_context(|| format!("{flag} requires a value"))
}
