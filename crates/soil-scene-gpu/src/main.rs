//! Render a captured `soil_scene` and present it to Canvas as a GPU lease.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Duration;

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
    let mut rgba_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scene" => scene_path = args.next(),
            "--canvas-socket" => socket = args.next(),
            "--source-id" => source_id = args.next(),
            "--hold" => hold = true,
            "--rgba" => rgba_path = args.next(),
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
    let bytes = std::fs::read(&scene_path)?;
    let present: soil_scene::Present = serde_json::from_slice(&bytes)?;
    let soil_scene::Present::Scene(scene) = present else {
        anyhow::bail!("--scene must contain a soil_scene scene");
    };
    let frame = soil_scene_gpu::render(&scene, size.0, size.1, Theme::default())?;
    if let Some(path) = rgba_path {
        std::fs::write(path, &frame.rgba)?;
    }
    present_frame(&socket, &source_id, &frame, hold)?;
    println!(
        "soil-scene-gpu: ok source={} {}x{} nodes={}",
        source_id,
        size.0,
        size.1,
        scene.nodes.len()
    );
    Ok(())
}

fn present_frame(socket: &str, source_id: &str, frame: &Frame, hold: bool) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
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
    let encoded = base64::engine::general_purpose::STANDARD.encode(&frame.rgba);
    let message = format!(
        r#"{{"kind":"gpu_frame","source_id":"{source_id}","width":{},"height":{},"revision":1,"rgba":"{encoded}"}}"#,
        frame.width, frame.height
    );
    writeln!(stream, "{message}")?;
    stream.flush()?;
    if hold {
        std::thread::sleep(Duration::from_secs(60));
    }
    Ok(())
}
