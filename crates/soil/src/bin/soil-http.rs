//! Run one component that imports `soil:http` and exports `run(args)`.
//!
//! ```sh
//! soil-http --allow https://example.com component.wasm https://example.com/feed.xml
//! ```

use std::{env, fs, process::ExitCode, sync::Arc, time::Duration};

#[cfg(unix)]
use soil::http_host::GeodeCredentials;
use soil::http_host::{BearerCredential, HttpHost, HttpPolicy, HttpRuntime};

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
    let mut bearer = None;
    let mut geode_socket = None;
    let mut geode_purpose = None;
    while let Some(arg) = args.peek() {
        match arg.as_str() {
            "--allow" => {
                args.next();
                origins.push(args.next().ok_or("--allow requires ORIGIN")?);
            }
            "--bearer-env" => {
                args.next();
                let name = args.next().ok_or("--bearer-env requires a VARIABLE")?;
                let token =
                    env::var(&name).map_err(|_| format!("--bearer-env: {name} is not set"))?;
                bearer = Some(BearerCredential::new(token));
            }
            "--geode-socket" => {
                args.next();
                geode_socket = Some(args.next().ok_or("--geode-socket requires a PATH")?);
            }
            "--geode-purpose" => {
                args.next();
                geode_purpose = Some(args.next().ok_or("--geode-purpose requires a PURPOSE")?);
            }
            _ => break,
        }
    }
    if origins.is_empty() {
        return Err(
            "usage: soil-http --allow ORIGIN [--allow ORIGIN]... [--bearer-env VARIABLE | --geode-socket PATH --geode-purpose PURPOSE] COMPONENT.wasm [ARGS...]".into(),
        );
    }
    if bearer.is_some() && geode_socket.is_some() {
        return Err("--bearer-env and --geode-socket are mutually exclusive".into());
    }
    if geode_socket.is_some() != geode_purpose.is_some() {
        return Err("--geode-socket and --geode-purpose must be used together".into());
    }
    let path = args.next().ok_or("missing COMPONENT.wasm")?;
    let guest_args: Vec<String> = args.collect();
    let bytes = fs::read(&path).map_err(|error| format!("read {path}: {error}"))?;
    let policy = HttpPolicy::new(origins, 16 * 1024 * 1024, Duration::from_secs(30))
        .map_err(|error| format!("policy: {error:#}"))?;
    let runtime = HttpRuntime::new().map_err(|error| format!("runtime: {error:#}"))?;
    let host = if let Some(credential) = bearer {
        HttpHost::new(policy).with_credentials(Arc::new(credential))
    } else if let (Some(socket), Some(purpose)) = (geode_socket, geode_purpose) {
        #[cfg(unix)]
        {
            HttpHost::new(policy).with_credentials(Arc::new(GeodeCredentials::new(socket, purpose)))
        }
        #[cfg(not(unix))]
        {
            return Err("--geode-socket is only available on Unix hosts".into());
        }
    } else {
        HttpHost::new(policy)
    };
    runtime
        .run_component(&bytes, host, &guest_args)
        .map_err(|error| format!("{error:#}"))
}
