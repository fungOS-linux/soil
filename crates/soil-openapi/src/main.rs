//! Write the bridge's OpenAPI document and TypeScript client.
//!
//! ```sh
//! cargo run -p soil-openapi -- --package soil:records --out crates/soil-web-bridge/generated
//! ```
//!
//! The artifacts are derived, never edited. A test regenerates them and fails on
//! a difference, so "regenerate instead of patching" is enforced rather than
//! remembered.

use std::fs;
use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Generate OpenAPI and TypeScript from the canonical Soil WIT"
)]
struct Args {
    /// Package to describe, e.g. `soil:records`
    #[arg(long, default_value = "soil:records")]
    package: String,

    /// Directory for `openapi.json` and `client.ts`
    #[arg(long, default_value = "generated")]
    out: PathBuf,

    /// Print the document and the client instead of writing files
    #[arg(long)]
    stdout: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let api = soil_openapi::describe(&args.package)?;
    let (document, client) = soil_openapi::artifacts(&api)?;

    if args.stdout {
        print!("{document}{client}");
        return Ok(());
    }

    fs::create_dir_all(&args.out)?;
    fs::write(args.out.join("openapi.json"), document)?;
    fs::write(args.out.join("client.ts"), client)?;
    println!(
        "{} {}: wrote {} and {}",
        api.package,
        api.version,
        args.out.join("openapi.json").display(),
        args.out.join("client.ts").display()
    );
    Ok(())
}
