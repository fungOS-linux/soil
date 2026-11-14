//! Run one component that imports `wasi:webgpu` and exports `run`.
//!
//! Usage:
//!   soil-webgpu <component.wasm>
//!   soil-webgpu --frame FILE.bgr [--size WxH] [--agents demo] <component.wasm>

use std::{env, fs, path::Path, process::ExitCode};

fn main() -> ExitCode {
    let mut frame_path = None;
    let mut size = (480, 640);
    let mut spores_root = None;
    let mut agents = None;
    let mut acp_cmd = None;
    let mut keys: Vec<String> = Vec::new();
    let mut seed: Vec<(String, String)> = Vec::new();
    let mut escalate_after: Option<u64> = None;
    let mut escalate_to: Option<String> = None;
    let mut escalate_grace: Option<u64> = None;
    let mut view = None;
    let mut agent = None;
    let mut store_origin = None;
    let mut component = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frame" => {
                let Some(path) = args.next() else {
                    eprintln!("--frame requires FILE.bgr");
                    return ExitCode::FAILURE;
                };
                frame_path = Some(path);
            }
            "--size" => {
                let Some(value) = args.next() else {
                    eprintln!("--size requires WxH");
                    return ExitCode::FAILURE;
                };
                let Some((width, height)) = value.split_once('x') else {
                    eprintln!("--size requires WxH");
                    return ExitCode::FAILURE;
                };
                let (Ok(width), Ok(height)) = (width.parse(), height.parse()) else {
                    eprintln!("--size requires positive integers");
                    return ExitCode::FAILURE;
                };
                if width == 0 || height == 0 {
                    eprintln!("--size requires positive integers");
                    return ExitCode::FAILURE;
                }
                size = (width, height);
            }
            "--spores-root" => {
                let Some(path) = args.next() else {
                    eprintln!("--spores-root requires a directory");
                    return ExitCode::FAILURE;
                };
                spores_root = Some(path);
            }
            "--agents" => {
                // Naming the source explicitly keeps a capture honest: a frame of
                // the demo cast is not a frame of a running Operator session.
                let Some(value) = args.next() else {
                    eprintln!("--agents requires a source (demo, acp)");
                    return ExitCode::FAILURE;
                };
                if value != "demo" && value != "acp" {
                    eprintln!("--agents: unknown source {value:?} (demo, acp)");
                    return ExitCode::FAILURE;
                }
                agents = Some(value);
            }
            "--escalate-after" => {
                let Some(value) = args.next() else {
                    eprintln!("--escalate-after requires seconds");
                    return ExitCode::FAILURE;
                };
                match value.parse() {
                    Ok(seconds) => escalate_after = Some(seconds),
                    Err(_) => {
                        eprintln!("--escalate-after requires seconds");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--escalate-to" => {
                let Some(value) = args.next() else {
                    eprintln!("--escalate-to requires an agent id");
                    return ExitCode::FAILURE;
                };
                escalate_to = Some(value);
            }
            "--escalate-grace" => {
                let Some(value) = args.next() else {
                    eprintln!("--escalate-grace requires seconds");
                    return ExitCode::FAILURE;
                };
                match value.parse() {
                    Ok(seconds) => escalate_grace = Some(seconds),
                    Err(_) => {
                        eprintln!("--escalate-grace requires seconds");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--key" => {
                let Some(value) = args.next() else {
                    eprintln!("--key requires a key name");
                    return ExitCode::FAILURE;
                };
                keys.push(value);
            }
            "--bus-seed" => {
                let Some(path) = args.next() else {
                    eprintln!("--bus-seed needs a file of `kind<TAB>body` lines");
                    return ExitCode::FAILURE;
                };
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        for line in text.lines().filter(|line| !line.trim().is_empty()) {
                            match line.split_once('\t') {
                                Some((kind, body)) => {
                                    seed.push((kind.to_string(), body.to_string()));
                                }
                                None => {
                                    eprintln!("--bus-seed: {path}: line is not kind<TAB>body");
                                    return ExitCode::FAILURE;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        eprintln!("--bus-seed: {path}: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--acp-cmd" => {
                let Some(value) = args.next() else {
                    eprintln!("--acp-cmd requires a command");
                    return ExitCode::FAILURE;
                };
                acp_cmd = Some(value);
            }
            "--view" => {
                let Some(value) = args.next() else {
                    eprintln!("--view requires board|roster|stage|focus");
                    return ExitCode::FAILURE;
                };
                view = Some(value);
            }
            "--agent" => {
                let Some(value) = args.next() else {
                    eprintln!("--agent requires an agent id");
                    return ExitCode::FAILURE;
                };
                agent = Some(value);
            }
            "--store-origin" => {
                let Some(origin) = args.next() else {
                    eprintln!("--store-origin requires a URL");
                    return ExitCode::FAILURE;
                };
                store_origin = Some(origin);
            }
            _ => {
                if component.replace(arg).is_some() {
                    eprintln!(
                        "usage: soil-webgpu [--frame FILE.bgr] [--size WxH] [--spores-root DIR] [--store-origin URL] [--agents demo|acp] [--acp-cmd CMD] <component.wasm>"
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    let Some(path) = component else {
        eprintln!(
            "usage: soil-webgpu [--frame FILE.bgr] [--size WxH] [--spores-root DIR] [--store-origin URL] [--agents demo|acp] [--acp-cmd CMD] <component.wasm>"
        );
        return ExitCode::FAILURE;
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = if frame_path.is_some() {
        let catalog = spores_root.as_ref().map(|root| {
            Box::new(soil_webgpu::InstallCatalog::new(root.clone()))
                as Box<dyn soil_webgpu::CatalogHost>
        });
        let cast: Option<Box<dyn soil_agent_host::AgentBackend>> = match agents.as_deref() {
            None => None,
            Some("demo") => Some(Box::new(
                soil_agent_host::FixtureAgents::new().bind(agent.clone(), view.clone()),
            )),
            Some("acp") => {
                #[cfg(feature = "acp")]
                {
                    let command = acp_cmd.clone().unwrap_or_else(|| "delta-acp".to_string());
                    match soil_webgpu::acp::AcpBackend::spawn(&command) {
                        Ok(backend) => Some(Box::new(backend.bind(agent.clone(), view.clone()))),
                        Err(error) => {
                            eprintln!("acp: {error}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                #[cfg(not(feature = "acp"))]
                {
                    eprintln!(
                        "--agents acp needs the `acp` feature: cargo build -p soil-webgpu --features acp"
                    );
                    return ExitCode::FAILURE;
                }
            }
            Some(other) => {
                eprintln!("--agents: unknown source {other:?} (demo, acp)");
                return ExitCode::FAILURE;
            }
        };
        // A prompt with no deadline is a deadlock; the policy is configuration,
        // so a host can be pointed at a PM without a rebuild.
        let mut policy = soil_agent_host::escalation::Escalation::from_environment();
        if let Some(seconds) = escalate_after {
            policy.after = std::time::Duration::from_secs(seconds);
        }
        if let Some(seconds) = escalate_grace {
            policy.grace = std::time::Duration::from_secs(seconds);
        }
        if let Some(to) = escalate_to.clone() {
            policy.to = Some(to);
        }
        let configured = policy.to.is_some()
            || std::env::var("SOIL_ESCALATE_AFTER_SECS").is_ok()
            || std::env::var("SOIL_ESCALATE_GRACE_SECS").is_ok();
        let cast: Option<Box<dyn soil_agent_host::AgentBackend>> = match cast {
            Some(backend) if configured => Some(Box::new(
                soil_agent_host::escalation::Escalating::new(backend, policy),
            )),
            other => other,
        };

        match soil_webgpu::run_component_capture_with_keys(
            &bytes,
            size.0,
            size.1,
            catalog,
            store_origin.clone(),
            cast,
            keys.clone(),
            seed.clone(),
        ) {
            Ok((status, frame)) => {
                if status != 0 {
                    eprintln!("webgpu: guest status {status}");
                    return ExitCode::FAILURE;
                }
                let Some(frame) = frame else {
                    eprintln!("webgpu: guest did not present a Canvas frame");
                    return ExitCode::FAILURE;
                };
                match write_bgr(frame_path.as_deref().expect("frame path"), &frame) {
                    Ok(()) => {
                        println!(
                            "webgpu: ok frame={} width={} height={} format=bgr24",
                            frame_path.as_deref().unwrap_or_default(),
                            frame.width,
                            frame.height
                        );
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    } else {
        soil_webgpu::run_component_blocking(&bytes).map(|status| {
            if status == 0 {
                println!("webgpu: ok");
            } else {
                eprintln!("webgpu: guest status {status}");
            }
        })
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("webgpu: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn write_bgr(path: &str, frame: &soil_webgpu::CanvasFrame) -> anyhow::Result<()> {
    let path = Path::new(path);
    let mut bgr = Vec::with_capacity(frame.width as usize * frame.height as usize * 3);
    for pixel in frame.rgba.as_chunks::<4>().0 {
        bgr.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, bgr)?;
    fs::rename(temporary, path)?;
    Ok(())
}
