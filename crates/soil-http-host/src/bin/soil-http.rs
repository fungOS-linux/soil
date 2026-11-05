//! Run one component that imports `soil:http` and exports `run(args)`.
//!
//! ```sh
//! soil-http --allow https://example.com component.wasm https://example.com/feed.xml
//! ```

use std::{env, fs, process::ExitCode, time::Duration};

use soil_http_host::{HttpHost, HttpPolicy, HttpRuntime};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-http: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1).peekable();
    let mut origins = Vec::new();
    while let Some(arg) = args.peek() {
        if arg != "--allow" {
            break;
        }
        args.next();
        origins.push(args.next().ok_or("--allow requires ORIGIN")?);
    }
    if origins.is_empty() {
        return Err(
            "usage: soil-http --allow ORIGIN [--allow ORIGIN]... COMPONENT.wasm [ARGS...]".into(),
        );
    }
    let path = args.next().ok_or("missing COMPONENT.wasm")?;
    let guest_args: Vec<String> = args.collect();
    let bytes = fs::read(&path).map_err(|error| format!("read {path}: {error}"))?;
    let policy = HttpPolicy::new(origins, 16 * 1024 * 1024, Duration::from_secs(30))
        .map_err(|error| format!("policy: {error:#}"))?;
    let runtime = HttpRuntime::new().map_err(|error| format!("runtime: {error:#}"))?;
    runtime
        .run_component(&bytes, HttpHost::new(policy), &guest_args)
        .map_err(|error| format!("{error:#}"))
}
