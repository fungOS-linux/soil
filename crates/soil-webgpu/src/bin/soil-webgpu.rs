//! Run one component that imports `wasi:webgpu` and exports `run`.
//!
//! Usage:
//!   soil-webgpu <component.wasm>
//!   soil-webgpu --frame FILE.bgr [--size WxH] <component.wasm>
//!   soil-webgpu --canvas-socket PATH --source-id ID [--size WxH] <component.wasm>

use std::{env, fs, path::Path, process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let mut frame_path = None;
    let mut canvas_socket = None;
    let mut source_id = None;
    let mut hold = false;
    let mut size = (480, 640);
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
            "--canvas-socket" => {
                let Some(path) = args.next() else {
                    eprintln!("--canvas-socket requires PATH");
                    return ExitCode::FAILURE;
                };
                canvas_socket = Some(path);
            }
            "--source-id" => {
                let Some(id) = args.next() else {
                    eprintln!("--source-id requires ID");
                    return ExitCode::FAILURE;
                };
                source_id = Some(id);
            }
            "--hold" => hold = true,
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
            _ => {
                if component.replace(arg).is_some() {
                    eprintln!(
                        "usage: soil-webgpu [--frame FILE.bgr | --canvas-socket PATH --source-id ID] [--size WxH] <component.wasm>"
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    let Some(path) = component else {
        eprintln!(
            "usage: soil-webgpu [--frame FILE.bgr | --canvas-socket PATH --source-id ID] [--size WxH] <component.wasm>"
        );
        return ExitCode::FAILURE;
    };
    if canvas_socket.is_some() && source_id.is_none() {
        eprintln!("--canvas-socket needs --source-id");
        return ExitCode::FAILURE;
    }
    if canvas_socket.is_some() && frame_path.is_some() {
        eprintln!("--canvas-socket and --frame are mutually exclusive");
        return ExitCode::FAILURE;
    }
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = if let Some(socket) = canvas_socket {
        let source = source_id.expect("checked above");
        let linger = hold.then_some(Duration::from_secs(60));
        match soil_webgpu::run_component_socket(
            &bytes,
            socket,
            source.clone(),
            size.0,
            size.1,
            linger,
        ) {
            Ok(status) if status == 0 => {
                println!("webgpu: ok canvas source={source}");
                Ok(())
            }
            Ok(status) => Err(anyhow::anyhow!("guest status {status}")),
            Err(error) => Err(error),
        }
    } else if frame_path.is_some() {
        match soil_webgpu::run_component_capture(&bytes, size.0, size.1) {
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
