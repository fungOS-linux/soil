//! Render a captured `soil_scene` and present it to Canvas as a GPU lease.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use soil_scene_gpu::{Frame, Theme};

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-scene-gpu: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut scene_path = None;
    let mut socket = None;
    let mut source_id = None;
    let mut size = (1200, 900);
    let mut hold = false;
    let mut watch = false;
    let mut fps = 30u32;
    let mut rgba_path = None;
    let mut input_out = None;
    let mut base64 = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scene" => scene_path = args.next(),
            "--canvas-socket" => socket = args.next(),
            "--source-id" => source_id = args.next(),
            "--hold" => hold = true,
            "--watch" => watch = true,
            "--fps" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("--fps requires a number");
                };
                fps = value.parse().map_err(|_| anyhow::anyhow!("invalid fps"))?;
                fps = fps.clamp(1, 120);
            }
            "--rgba" => rgba_path = args.next(),
            "--input-out" => input_out = args.next(),
            "--base64" => base64 = true,
            "--size" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("--size requires WxH");
                };
                let (w, h) = value
                    .split_once('x')
                    .ok_or_else(|| anyhow::anyhow!("--size requires WxH"))?;
                size = (
                    w.parse().map_err(|_| anyhow::anyhow!("invalid width"))?,
                    h.parse().map_err(|_| anyhow::anyhow!("invalid height"))?,
                );
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let scene_path = scene_path.ok_or_else(|| anyhow::anyhow!("missing --scene"))?;
    let socket = socket.ok_or_else(|| anyhow::anyhow!("missing --canvas-socket"))?;
    let source_id = source_id.ok_or_else(|| anyhow::anyhow!("missing --source-id"))?;

    let mut last_modified = modified(&scene_path);
    let mut revision = 1u64;
    let mut scene = read_scene(&scene_path)?;
    let mut frame = soil_scene_gpu::render(&scene, size.0, size.1, Theme::default())?;
    if let Some(path) = rgba_path {
        std::fs::write(path, &frame.rgba)?;
    }

    let mut stream = UnixStream::connect(&socket)?;
    if let Some(path) = input_out {
        let reader = stream.try_clone()?;
        std::thread::Builder::new()
            .name("soil-scene-input".into())
            .spawn(move || read_inputs(reader, path))?;
    }
    send_hello(&mut stream, &source_id, &frame)?;
    send_frame(&mut stream, &source_id, &frame, revision, !base64)?;
    stream.flush()?;
    println!(
        "soil-scene-gpu: ok source={} {}x{} nodes={}",
        source_id,
        size.0,
        size.1,
        scene.nodes.len()
    );

    if watch {
        let interval = Duration::from_millis((1000 / fps.max(1)) as u64);
        loop {
            std::thread::sleep(interval);
            let Some(now) = modified(&scene_path) else {
                continue;
            };
            if last_modified.is_some_and(|previous| now <= previous) {
                continue;
            }
            let next_scene = match read_scene(&scene_path) {
                Ok(scene) => scene,
                Err(error) => {
                    eprintln!("soil-scene-gpu: scene reload failed: {error:#}");
                    continue;
                }
            };
            let next_frame =
                match soil_scene_gpu::render(&next_scene, size.0, size.1, Theme::default()) {
                    Ok(frame) => frame,
                    Err(error) => {
                        eprintln!("soil-scene-gpu: scene render failed: {error:#}");
                        continue;
                    }
                };
            revision += 1;
            if let Err(error) = send_frame(&mut stream, &source_id, &next_frame, revision, !base64)
            {
                anyhow::bail!("publish frame {revision}: {error}");
            }
            if let Err(error) = stream.flush() {
                anyhow::bail!("flush frame {revision}: {error}");
            }
            last_modified = Some(now);
            scene = next_scene;
            frame = next_frame;
            let _ = &scene;
            let _ = &frame;
        }
    } else if hold {
        std::thread::sleep(Duration::from_secs(60));
    }
    Ok(())
}

fn read_inputs(stream: UnixStream, path: String) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let kind = message.get("kind").and_then(|value| value.as_str());
        if !matches!(kind, Some("pointer" | "scroll" | "key")) {
            continue;
        }
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
            continue;
        };
        if writeln!(file, "{line}").is_err() {
            break;
        }
    }
}

fn modified(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

fn read_scene(path: &str) -> anyhow::Result<soil_scene::Scene> {
    let bytes = std::fs::read(path)?;
    let present: soil_scene::Present = serde_json::from_slice(&bytes)?;
    let soil_scene::Present::Scene(scene) = present else {
        anyhow::bail!("--scene must contain a soil_scene scene");
    };
    Ok(scene)
}

fn send_hello(stream: &mut UnixStream, source_id: &str, frame: &Frame) -> anyhow::Result<()> {
    let hello = format!(
        r#"{{"kind":"hello","surface_id":"{source_id}","title":"{source_id}","width":{},"height":{}}}"#,
        frame.width, frame.height
    );
    writeln!(stream, "{hello}")?;
    let lease = format!(
        r#"{{"kind":"present","payload":{{"kind":"gpu_lease","source_id":"{source_id}","width":{},"height":{},"revision":1}}}}"#,
        frame.width, frame.height
    );
    writeln!(stream, "{lease}")?;
    Ok(())
}

fn send_frame(
    stream: &mut UnixStream,
    source_id: &str,
    frame: &Frame,
    revision: u64,
    shared: bool,
) -> anyhow::Result<()> {
    if shared {
        let shared = soil_scene::shm::publish(frame.width, frame.height, revision, &frame.rgba)?;
        let message = format!(
            r#"{{"kind":"gpu_frame_shm","source_id":"{source_id}","path":{},"width":{},"height":{},"revision":{}}}"#,
            serde_json::to_string(&shared.name)?,
            frame.width,
            frame.height,
            shared.revision
        );
        writeln!(stream, "{message}")?;
    } else {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&frame.rgba);
        let message = format!(
            r#"{{"kind":"gpu_frame","source_id":"{source_id}","width":{},"height":{},"revision":{},"rgba":"{encoded}"}}"#,
            frame.width, frame.height, revision
        );
        writeln!(stream, "{message}")?;
    }
    Ok(())
}
