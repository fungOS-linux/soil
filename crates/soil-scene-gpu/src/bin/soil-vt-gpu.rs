//! Render a terminal through the GPU scene path.
//!
//! `--text` renders one snapshot. `--shell` or `--command` runs a real pty,
//! feeds bytes into `soil_vt::Host`, and republishes a shared frame whenever the
//! grid generation changes.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{TryRecvError, channel};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use soil_scene_gpu::Theme;

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-vt-gpu: {error:#}");
        std::process::exit(1);
    }
}

#[derive(Default)]
struct Options {
    text: Option<String>,
    command: Vec<String>,
    shell: bool,
    socket: Option<String>,
    source_id: Option<String>,
    cols: u16,
    rows: u16,
    size: (u32, u32),
    hold: bool,
    rgba: Option<String>,
}

fn parse() -> anyhow::Result<Options> {
    let mut options = Options {
        cols: 80,
        rows: 24,
        size: (960, 540),
        ..Default::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--text" => options.text = args.next(),
            "--shell" => options.shell = true,
            "--command" => {
                options.command = args.by_ref().collect();
                break;
            }
            "--canvas-socket" => options.socket = args.next(),
            "--source-id" => options.source_id = args.next(),
            "--cols" => {
                options.cols = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--cols needs a number"))?
                    .parse()?
            }
            "--rows" => {
                options.rows = args
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
                options.size = (w.parse()?, h.parse()?);
            }
            "--hold" => options.hold = true,
            "--rgba" => options.rgba = args.next(),
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    Ok(options)
}

fn run() -> anyhow::Result<()> {
    let mut options = parse()?;
    let socket = options
        .socket
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing --canvas-socket"))?;
    let source_id = options
        .source_id
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing --source-id"))?;
    if options.shell || !options.command.is_empty() {
        if options.command.is_empty() {
            options.command = vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())];
        }
        return run_pty(&socket, &source_id, &options);
    }
    let text = options.text.take().unwrap_or_default();
    let mut session = soil_vt::Session::new(options.cols, options.rows, 2_000)?;
    session.write(text.as_bytes());
    let snapshot = session.snapshot()?;
    let frame = soil_scene_gpu::terminal::render_terminal(
        &snapshot,
        options.size.0,
        options.size.1,
        Theme::default(),
    )?;
    if let Some(path) = options.rgba {
        std::fs::write(path, &frame.rgba)?;
    }
    let mut stream = present_stream(&socket, &source_id, &frame)?;
    if options.hold {
        std::thread::sleep(Duration::from_secs(60));
    }
    let _ = stream.flush();
    println!(
        "soil-vt-gpu: ok source={source_id} {}x{} cursor={:?}",
        options.size.0, options.size.1, snapshot.cursor
    );
    Ok(())
}

fn run_pty(socket: &str, source_id: &str, options: &Options) -> anyhow::Result<()> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: options.rows,
        cols: options.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut builder = CommandBuilder::new(&options.command[0]);
    builder.args(&options.command[1..]);
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    builder.env("TERM_PROGRAM", "soil-vt-gpu");
    let mut child = pair.slave.spawn_command(builder)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let host = soil_vt::Host::spawn(options.cols, options.rows, 2_000)?;
    let mut renderer = soil_scene_gpu::Renderer::new()?;
    renderer.set_font(soil_scene_gpu::load_mono_font());
    let (sender, receiver) = channel::<Vec<u8>>();
    let reader_host = host.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    reader_host.write(&buffer[..n]);
                    let _ = sender.send(buffer[..n].to_vec());
                }
            }
        }
    });
    let first = host.snapshot();
    let first_scene = soil_scene_gpu::terminal::terminal_scene(
        &first,
        options.size.0,
        options.size.1,
        Theme::terminal(),
    );
    let first_frame = renderer.render(
        &first_scene,
        options.size.0,
        options.size.1,
        Theme::default(),
    )?;
    let mut stream = Some(present_stream(socket, source_id, &first_frame)?);
    let mut revision = 2u64;
    let mut last_generation = host.generation();
    let mut last_draw = Instant::now();
    loop {
        for _ in 0..16 {
            match receiver.try_recv() {
                Ok(bytes) => {
                    let responses = host.drain_responses();
                    if !responses.is_empty() {
                        let _ = writer.write_all(&responses);
                    }
                    let _ = bytes;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        let generation = host.generation();
        if generation != last_generation && last_draw.elapsed() >= Duration::from_millis(30) {
            let snapshot = host.snapshot();
            let scene = soil_scene_gpu::terminal::terminal_scene(
                &snapshot,
                options.size.0,
                options.size.1,
                Theme::default(),
            );
            let frame =
                renderer.render(&scene, options.size.0, options.size.1, Theme::default())?;
            let shared =
                soil_scene::shm::publish(frame.width, frame.height, revision, &frame.rgba)?;
            let line = format!(
                r#"{{"kind":"gpu_frame_shm","source_id":"{source_id}","path":{},"width":{},"height":{},"revision":{revision}}}"#,
                serde_json::to_string(&shared.name)?,
                frame.width,
                frame.height
            );
            let stream = stream.as_mut().expect("stream initialized");
            writeln!(stream, "{line}")?;
            stream.flush()?;
            revision = revision.saturating_add(1);
            last_generation = generation;
            last_draw = Instant::now();
        }
        if child.try_wait()?.is_some() && receiver.try_recv().is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    if options.hold {
        std::thread::sleep(Duration::from_secs(60));
    }
    Ok(())
}

fn present_stream(
    socket: &str,
    source_id: &str,
    frame: &soil_scene_gpu::Frame,
) -> anyhow::Result<UnixStream> {
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
        r#"{{"kind":"gpu_frame_shm","source_id":"{source_id}","path":{},"width":{},"height":{},"revision":1}}"#,
        serde_json::to_string(&shared.name)?,
        frame.width,
        frame.height
    )?;
    stream.flush()?;
    Ok(stream)
}
