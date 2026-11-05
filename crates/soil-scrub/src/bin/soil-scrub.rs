//! One-shot scrubber filter.
//!
//! This is a debugging and gateway-building tool, not the long-lived session
//! itself. The real gateway keeps one `Scrubber` alive and restores only at an
//! authorized tool boundary.

use std::io::{self, Read};

use soil_scrub::Scrubber;

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-scrub: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut text = None;
    let mut scopes = vec!["display".to_string()];
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--text" => text = Some(args.next().ok_or("--text needs a value")?),
            "--scope" => {
                let scope = args.next().ok_or("--scope needs a value")?;
                if scopes == ["display"] {
                    scopes.clear();
                }
                scopes.push(scope);
            }
            "--help" | "-h" => {
                println!(
                    "usage: soil-scrub [--text TEXT] [--scope SCOPE]...\nreads stdin when --text is absent"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let input = match text {
        Some(text) => text,
        None => {
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .map_err(|error| error.to_string())?;
            input
        }
    };
    let scopes: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let mut scrubber = Scrubber::new();
    print!("{}", scrubber.scrub_with_scopes(&input, &scopes));
    Ok(())
}
