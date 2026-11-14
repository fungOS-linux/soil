//! Run one component that imports `wasi:webgpu` and exports `run`.
//!
//! Usage:
//!   soil-webgpu <component.wasm>
//!   soil-webgpu --frame FILE.bgr [--size WxH] [--agents demo] <component.wasm>
//!   soil-webgpu --frame FILE.bgr --root VAULT --records DIR \
//!     --book BOOK --collection NAME --actor WHO <component.wasm>
//!
//! `--root`, `--records`, `--book`, `--collection` and `--actor` are the grants
//! a document-shaped Spore asks for: one workspace to read, a book to write
//! pending entities into, and the identity the host attests for them.

use std::{env, fs, path::Path, path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

#[cfg(unix)]
use soil::http_host::GeodeCredentials;
use soil::http_host::HttpPolicy;
use soil_webgpu::{HttpAccess, run_component_capture_with_records};

fn main() -> ExitCode {
    let mut frame_path = None;
    let mut fb_path: Option<String> = None;
    let mut records_dir: Option<PathBuf> = None;
    let mut vfs_root: Option<PathBuf> = None;
    let mut records_root: Option<PathBuf> = None;
    let mut book: Option<String> = None;
    let mut collection: Option<String> = None;
    let mut actor: Option<String> = None;
    let mut size = (480, 640);
    let mut spores_roots: Vec<PathBuf> = Vec::new();
    let mut origins = Vec::new();
    let mut store_origin: Option<String> = None;
    let mut geode_socket = None;
    let mut geode_purpose = None;
    let mut agents = None;
    let mut view = None;
    let mut agent = None;
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
            "--fb" => {
                let Some(path) = args.next() else {
                    eprintln!("--fb requires a framebuffer device, e.g. /dev/fb0");
                    return ExitCode::FAILURE;
                };
                fb_path = Some(path);
            }
            "--records-dir" => {
                let Some(path) = args.next() else {
                    eprintln!("--records-dir requires a directory");
                    return ExitCode::FAILURE;
                };
                records_dir = Some(PathBuf::from(path));
            }
            "--root" => {
                let Some(path) = args.next() else {
                    eprintln!("--root requires a directory");
                    return ExitCode::FAILURE;
                };
                vfs_root = Some(PathBuf::from(path));
            }
            "--records" => {
                let Some(path) = args.next() else {
                    eprintln!("--records requires a directory");
                    return ExitCode::FAILURE;
                };
                records_root = Some(PathBuf::from(path));
            }
            "--book" => {
                let Some(value) = args.next() else {
                    eprintln!("--book requires a name");
                    return ExitCode::FAILURE;
                };
                book = Some(value);
            }
            "--collection" => {
                let Some(value) = args.next() else {
                    eprintln!("--collection requires a name");
                    return ExitCode::FAILURE;
                };
                collection = Some(value);
            }
            "--actor" => {
                let Some(value) = args.next() else {
                    eprintln!("--actor requires a principal name");
                    return ExitCode::FAILURE;
                };
                actor = Some(value);
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
                spores_roots.push(PathBuf::from(path));
            }
            "--allow-origin" => {
                let Some(origin) = args.next() else {
                    eprintln!("--allow-origin requires a URL");
                    return ExitCode::FAILURE;
                };
                origins.push(origin);
            }
            "--store-origin" => {
                let Some(origin) = args.next() else {
                    eprintln!("--store-origin requires a URL");
                    return ExitCode::FAILURE;
                };
                store_origin = Some(origin);
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
            "--geode-socket" => {
                let Some(path) = args.next() else {
                    eprintln!("--geode-socket requires a path");
                    return ExitCode::FAILURE;
                };
                geode_socket = Some(PathBuf::from(path));
            }
            "--geode-purpose" => {
                let Some(purpose) = args.next() else {
                    eprintln!("--geode-purpose requires a purpose");
                    return ExitCode::FAILURE;
                };
                geode_purpose = Some(purpose);
            }
            _ => {
                if component.replace(arg).is_some() {
                    eprintln!(
                        "usage: soil-webgpu [--frame FILE.bgr] [--size WxH] [--records-dir DIR] [--spores-root DIR]... [--allow-origin URL]... [--store-origin URL] [--agents demo] [--view NAME] [--agent ID] [--geode-socket PATH --geode-purpose PURPOSE] <component.wasm>"
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    let Some(path) = component else {
        eprintln!(
            "usage: soil-webgpu [--frame FILE.bgr] [--size WxH] [--records-dir DIR] [--spores-root DIR]... [--allow-origin URL]... [--store-origin URL] [--agents demo] [--view NAME] [--agent ID] [--geode-socket PATH --geode-purpose PURPOSE] <component.wasm>"
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
    let result = if frame_path.is_some() || fb_path.is_some() {
        if records_dir.is_some()
            && (!origins.is_empty()
                || !spores_roots.is_empty()
                || store_origin.is_some()
                || agents.is_some())
        {
            eprintln!(
                "--records-dir alone cannot be combined with --allow-origin, --store-origin, --spores-root or --agents; pass --root or --actor to use the document grants"
            );
            return ExitCode::FAILURE;
        }
        let document_grants = vfs_root.is_some() || actor.is_some() || records_root.is_some();
        let result = if document_grants {
            let mut grants = soil_webgpu::Grants::new();
            grants.vfs = vfs_root.clone();
            if let Some(root) = records_root.clone() {
                let Some(collection) = collection.clone() else {
                    eprintln!("--records requires --collection: the guest names a collection");
                    return ExitCode::FAILURE;
                };
                grants.records = Some(soil_webgpu::RecordsAccess::book(
                    &root,
                    collection.clone(),
                    book.clone().unwrap_or_else(|| collection.clone()),
                    collection,
                ));
            }
            if let Some(actor) = actor.clone() {
                grants = grants.actor(actor);
            }
            soil_webgpu::run_component_capture_with_grants(&bytes, size.0, size.1, grants)
        } else if let Some(root) = records_dir.as_deref() {
            run_component_capture_with_records(&bytes, size.0, size.1, root)
        } else {
            let catalog = if spores_roots.is_empty() {
                None
            } else {
                Some(Box::new(soil_webgpu::InstallCatalog::with_roots(
                    spores_roots.clone(),
                )) as Box<dyn soil_webgpu::CatalogHost>)
            };
            let http = match build_http(origins, store_origin, geode_socket, geode_purpose) {
                Ok(http) => http,
                Err(error) => {
                    eprintln!("webgpu: {error}");
                    return ExitCode::FAILURE;
                }
            };
            let cast = agents.map(|_| {
                Box::new(soil_agent_host::FixtureAgents::new().bind(agent.clone(), view.clone()))
                    as Box<dyn soil_agent_host::AgentBackend>
            });
            match fb_path.as_deref() {
                Some(device) => run_on_framebuffer(&bytes, size, device, catalog, http, cast),
                None => soil_webgpu::run_component_capture_with(
                    &bytes, size.0, size.1, catalog, http, cast,
                ),
            }
        };
        match result {
            Ok((status, frame)) => {
                if status != 0 {
                    eprintln!("webgpu: guest status {status}");
                    return ExitCode::FAILURE;
                }
                if let Some(device) = fb_path.as_deref() {
                    println!(
                        "webgpu: ok fb={device} width={} height={} format=rgb565",
                        size.0, size.1
                    );
                    return ExitCode::SUCCESS;
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

fn build_http(
    origins: Vec<String>,
    store_origin: Option<String>,
    geode_socket: Option<PathBuf>,
    geode_purpose: Option<String>,
) -> Result<Option<HttpAccess>, String> {
    match (geode_socket.as_ref(), geode_purpose.as_ref()) {
        (Some(_), None) | (None, Some(_)) => {
            return Err("--geode-socket and --geode-purpose must be used together".into());
        }
        _ => {}
    }
    if origins.is_empty() {
        return match store_origin {
            Some(origin) => HttpAccess::store(origin)
                .map(Some)
                .map_err(|error| format!("http policy: {error:#}")),
            None => Ok(None),
        };
    }
    let policy = HttpPolicy::new(origins, 16 * 1024 * 1024, Duration::from_secs(30))
        .map_err(|error| format!("http policy: {error:#}"))?;
    let mut access = HttpAccess::new(policy);
    if let (Some(socket), Some(purpose)) = (geode_socket, geode_purpose) {
        #[cfg(unix)]
        {
            access = access.with_credentials(Arc::new(GeodeCredentials::new(socket, purpose)));
        }
        #[cfg(not(unix))]
        {
            let _ = (socket, purpose);
            return Err("--geode-socket is only available on Unix hosts".into());
        }
    }
    Ok(Some(access))
}

/// Present one Spore frame directly on a framebuffer device.
fn run_on_framebuffer(
    bytes: &[u8],
    size: (u32, u32),
    device: &str,
    catalog: Option<Box<dyn soil_webgpu::CatalogHost>>,
    http: Option<HttpAccess>,
    agents: Option<Box<dyn soil_agent_host::AgentBackend>>,
) -> anyhow::Result<(u32, Option<soil_webgpu::CanvasFrame>)> {
    let presenter = soil_webgpu::fb::FramebufferPresenter::open(device, size.0, size.1)?;
    let runtime = soil_webgpu::WebGpuRuntime::new()?;
    let mut setup = soil_webgpu::HostSetup::capture(Box::new(presenter));
    if let Some(catalog) = catalog {
        setup.catalog = catalog;
    }
    if let Some(agents) = agents {
        setup.agents = agents;
    }
    setup.http = http;
    let status = pollster::block_on(runtime.run_component_with(bytes, setup))?;
    Ok((status, None))
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
