//! Run a Lua Spore and write the document it draws.
//!
//! This is the whole "open a Spore" loop for a Lua package, minus a window: read
//! the package, run the view once against a snapshot, and either print what it
//! drew or hand the document to a renderer (`soil-scene-gpu --scene`).
//!
//! ```text
//! soil-lua check spores/divebar
//! soil-lua run spores/divebar --region top-bar --width 1200 --height 44 \
//!   --snapshot snapshot.json --out /tmp/bar.json
//! ```

use anyhow::{Context, Result, bail};
use soil_lua::{Spore, Theme};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-lua: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    if !matches!(command.as_str(), "run" | "check") {
        bail!(usage());
    }

    let mut root: Option<PathBuf> = None;
    let mut region: Option<String> = None;
    let mut width = 1200.0_f32;
    let mut height = 44.0_f32;
    let mut theme: Option<PathBuf> = None;
    let mut snapshot: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--region" => region = Some(value(&mut args, "--region")?),
            "--width" => width = parse(&value(&mut args, "--width")?, "--width")?,
            "--height" => height = parse(&value(&mut args, "--height")?, "--height")?,
            "--theme" => theme = Some(PathBuf::from(value(&mut args, "--theme")?)),
            "--snapshot" => snapshot = Some(PathBuf::from(value(&mut args, "--snapshot")?)),
            "--out" => out = Some(PathBuf::from(value(&mut args, "--out")?)),
            other if other.starts_with("--") => bail!("unknown flag {other:?}\n{}", usage()),
            other => root = Some(PathBuf::from(other)),
        }
    }

    let root = root.ok_or_else(|| anyhow::anyhow!(usage()))?;
    let spore = Spore::load_region(&root, region.as_deref())
        .with_context(|| format!("loading {}", root.display()))?;

    if command == "check" {
        println!(
            "{} {} target={} entry={}",
            spore.id(),
            spore.version(),
            soil_lua::TARGET,
            spore.entry()
        );
        return Ok(());
    }

    let theme = match theme {
        Some(path) => {
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            Theme::from_source(&source).with_context(|| format!("parsing {}", path.display()))?
        }
        None => Theme::new(),
    };
    let snapshot = match snapshot {
        Some(path) => {
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str(&source).with_context(|| format!("parsing {}", path.display()))?
        }
        None => serde_json::json!({}),
    };

    let present = spore
        .present(&snapshot, &theme, [width, height])
        .with_context(|| format!("rendering {}", spore.entry()))?;
    let nodes = match &present {
        soil_scene::Present::Scene(scene) => scene.nodes.len(),
        _ => 0,
    };
    if let Some(path) = out {
        let bytes = serde_json::to_vec_pretty(&present).context("serializing the scene")?;
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    println!(
        "{} {} {}: {} nodes at {}x{}",
        spore.id(),
        spore.version(),
        spore.entry(),
        nodes,
        width,
        height
    );
    Ok(())
}

fn usage() -> String {
    "usage: soil-lua run <package> [--region R] [--width W] [--height H]\n       \
     [--theme F] [--snapshot F] [--out F]\n       soil-lua check <package> [--region R]"
        .to_string()
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
}

fn parse(value: &str, flag: &str) -> Result<f32> {
    value
        .parse::<f32>()
        .with_context(|| format!("{flag} expects a number, not {value:?}"))
}
