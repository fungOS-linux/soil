//! Run one component that imports `wasi:webgpu` and exports `run`.
//!
//! Usage:
//!   soil-webgpu <component.wasm>
//!   soil-webgpu --frame FILE.bgr [--size WxH] [--agents demo] <component.wasm>
//!
//! A capture that needs the network names the one origin the guest may reach and,
//! if the guest asks for a credential, the environment variable holding it:
//!   soil-webgpu --frame f.bgr --store-origin http://127.0.0.1:23373 \
//!                --credential-env WEEPER_TOKEN --component-cache DIR component.wasm

use std::{env, fs, path::Path, process::ExitCode, sync::Arc};

use soil_webgpu::{BearerCredential, CaptureHost, CredentialProvider, NoCredentials};

fn main() -> ExitCode {
    let mut frame_path = None;
    let mut size = (480, 640);
    let mut spores_root = None;
    let mut agents = None;
    let mut view = None;
    let mut agent = None;
    let mut store_origin = None;
    let mut credential_env = None;
    let mut component_cache = None;
    let mut keys = Vec::new();
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
                // Only one cast exists so far. Naming it explicitly keeps a
                // capture honest: a frame of the demo cast is not a frame of a
                // running Operator session.
                let Some(value) = args.next() else {
                    eprintln!("--agents requires a cast name (demo)");
                    return ExitCode::FAILURE;
                };
                if value != "demo" {
                    eprintln!("--agents: unknown cast {value:?} (demo)");
                    return ExitCode::FAILURE;
                }
                agents = Some(value);
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
            // The token never reaches the guest, the component's arguments or
            // this process's `argv`: it is read here and stays in the host,
            // which injects it for the handle the guest names.
            "--credential-env" => {
                let Some(variable) = args.next() else {
                    eprintln!("--credential-env requires an environment variable name");
                    return ExitCode::FAILURE;
                };
                credential_env = Some(variable);
            }
            // Compiling a Spore is minutes; reading a cached compilation of the
            // same bytes is a read. Named by digest, so a rebuilt component
            // misses the cache instead of running the old one.
            "--component-cache" => {
                let Some(directory) = args.next() else {
                    eprintln!("--component-cache requires a directory");
                    return ExitCode::FAILURE;
                };
                component_cache = Some(std::path::PathBuf::from(directory));
            }
            // Deliver a key before the frame is taken. Repeatable, ordered, and
            // the guest decides when to stop: it exits on its own quit key.
            "--key" => {
                let Some(key) = args.next() else {
                    eprintln!("--key requires a key name, such as arrow-down or q");
                    return ExitCode::FAILURE;
                };
                keys.push(key);
            }
            _ => {
                if component.replace(arg).is_some() {
                    eprintln!(
                        "usage: soil-webgpu [--frame FILE.bgr] [--size WxH] [--spores-root DIR] [--store-origin URL] [--credential-env VAR] [--component-cache DIR] [--key NAME] [--agents demo] <component.wasm>"
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    let Some(path) = component else {
        eprintln!(
            "usage: soil-webgpu [--frame FILE.bgr] [--size WxH] [--spores-root DIR] [--store-origin URL] [--credential-env VAR] [--component-cache DIR] [--key NAME] [--agents demo] <component.wasm>"
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
        let cast = agents.map(|_| {
            Box::new(soil_agent_host::FixtureAgents::new().bind(agent.clone(), view.clone()))
                as Box<dyn soil_agent_host::AgentBackend>
        });
        let credentials: Arc<dyn CredentialProvider> = match credential_env {
            Some(variable) => match env::var(&variable) {
                Ok(token) if !token.is_empty() => Arc::new(BearerCredential::new(token)),
                _ => {
                    eprintln!("--credential-env: {variable} is unset or empty");
                    return ExitCode::FAILURE;
                }
            },
            None => Arc::new(NoCredentials),
        };
        match soil_webgpu::run_component_capture_with(
            &bytes,
            size.0,
            size.1,
            CaptureHost {
                catalog,
                agents: cast,
                store_origin: store_origin.clone(),
                credentials,
                component_cache: component_cache.clone(),
                keys: keys.clone(),
            },
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
