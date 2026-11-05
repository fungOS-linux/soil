//! Render a terminal snapshot through the GPU scene path.
//!
//! This first slice feeds the emulator from `--text` and presents one frame.
//! The PTY loop and input are the same pieces Canvas already owns; this proves
//! the cell grid, glyph atlas and shared-frame transport.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use soil_scene_gpu::Theme;

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-vt-gpu: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut text = String::new();
    let mut socket = None;
    let mut source_id = None;
    let mut cols = 80u16;
    let mut rows = 24u16;
    let mut size = (960u32, 540u32);
    let mut hold = false;
    let mut rgba_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--text" => {
                text = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--text needs a value"))?
            }
            "--canvas-socket" => socket = args.next(),
            "--source-id" => source_id = args.next(),
            "--cols" => {
                cols = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--cols needs a number"))?
                    .parse()?
            }
            "--rows" => {
                rows = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--rows needs a number"))?
                    .parse()?
            }
            "--size" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--size needs WxH"))?;
                let (w, h) = value
                    .split_once('x')
                    .ok_or_else(|| anyhow::anyhow!("--size needs WxH"))?;
                size = (w.parse()?, h.parse()?);
            }
            "--hold" => hold = true,
            "--rgba" => rgba_path = args.next(),
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let socket = socket.ok_or_else(|| anyhow::anyhow!("missing --canvas-socket"))?;
    let source_id = source_id.ok_or_else(|| anyhow::anyhow!("missing --source-id"))?;
    let mut session = soil_vt::Session::new(cols, rows, 2_000)?;
    session.write(text.as_bytes());
    let snapshot = session.snapshot()?;
    if std::env::var_os("SOIL_VT_DUMP").is_some() {
        for row in 0..snapshot.rows {
            eprintln!("{row:02}: {}", snapshot.row_text(row));
            for x in 0..snapshot.cols {
                if let Some(cell) = snapshot.cell(x, row) {
                    if !cell.text.is_empty() {
                        eprintln!("    {x:02} {:?} fg={:?} bg={:?}", cell.text, cell.fg, cell.bg);
                    }
                }
            }
        }
    }
    let frame =
        soil_scene_gpu::terminal::render_terminal(&snapshot, size.0, size.1, Theme::default())?;
    if let Some(path) = rgba_path {
        std::fs::write(path, &frame.rgba)?;
    }
    present_frame(&socket, &source_id, &frame, hold)?;
    println!(
        "soil-vt-gpu: ok source={} {}x{} cursor={:?}",
        source_id, size.0, size.1, snapshot.cursor
    );
    Ok(())
}

fn present_frame(
    socket: &str,
    source_id: &str,
    frame: &soil_scene_gpu::Frame,
    hold: bool,
) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    writeln!(
        stream,
        r#"{{"kind":"hello","surface_id":"{source_id}","title":"{source_id}","width":{},"height":{}}}"#,
        frame.width, frame.height
    )?;
    writeln!(
        stream,
        r#"{{"kind":"present","payload":{{"kind":"gpu_lease","source_id":"{source_id}","width":{},"height":{},"revision":1}}}}"#,
        frame.width, frame.height
    )?;
    let shared = soil_scene::shm::publish(frame.width, frame.height, 1, &frame.rgba)?;
    writeln!(
        stream,
        r#"{{"kind":"gpu_frame_shm","source_id":"{source_id}","path":{},"width":{},"height":{},"revision":{}}}"#,
        serde_json::to_string(&shared.name)?,
        frame.width,
        frame.height,
        shared.revision
    )?;
    stream.flush()?;
    if hold {
        std::thread::sleep(Duration::from_secs(60));
    }
    Ok(())
}
