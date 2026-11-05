//! A producer, shaped like a Spore, that presents into a Canvas window.
//!
//! It talks the `ui.sock` protocol: hello, then a payload from `soil-scene` on
//! every frame it wants drawn. No compositor code is linked, no native build is
//! needed to change what it draws -- which is the point of the payload being
//! data rather than an ABI.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use soil_scene::{Content, Ink, Node, Present, Rect, Scene, Text, Transform};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let surface = args.next().unwrap_or_else(|| "spore-demo".into());
    let path = std::env::var("CANVAS_UI_SOCKET").unwrap_or_else(|_| {
        format!(
            "{}/Library/Application Support/Dock/canvas/ui.sock",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    let stream = UnixStream::connect(&path)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream.try_clone()?);

    send(
        &mut writer,
        &serde_json::json!({
            "kind": "hello",
            "surface_id": surface,
            "title": "Spore demo",
            "width": 640,
            "height": 420,
        }),
    )?;
    let _ = read(&mut reader);

    // Pipelined, not lock-step: waiting for a reply after every frame adds the
    // round trip to the frame period, which is exactly the stutter a 60 Hz
    // producer must not have. Replies are drained when they arrive; only an
    // error stops the loop.
    reader.get_ref().set_nonblocking(true)?;
    let started = Instant::now();
    let mut last_signature = String::new();
    let mut next = Duration::ZERO;
    loop {
        let now = started.elapsed();
        if now < next {
            std::thread::sleep(next - now);
        }
        next += Duration::from_micros(16_667);
        if started.elapsed() > next {
            // A stalled frame must not make the next fifty fire at once.
            next = started.elapsed() + Duration::from_micros(16_667);
        }
        drain(&mut reader);
        let t = started.elapsed().as_secs_f32();
        let scene = frame(t);
        let signature = serde_json::to_string(&scene).unwrap_or_default();
        if signature != last_signature {
            send(
                &mut writer,
                &serde_json::json!({
                    "kind": "present",
                    "payload": Present::Scene(scene),
                }),
            )?;
            last_signature = signature;
        }
    }
}

/// Take whatever the host has said since the last frame, without waiting.
fn drain(reader: &mut BufReader<UnixStream>) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {
                if let Ok(reply) = serde_json::from_str::<serde_json::Value>(&line)
                    && reply.get("kind").and_then(|k| k.as_str()) == Some("error")
                {
                    eprintln!(
                        "present refused: {}",
                        reply.get("message").and_then(|m| m.as_str()).unwrap_or("?")
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(_) => return,
        }
    }
}

fn send(writer: &mut UnixStream, value: &serde_json::Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value).unwrap_or_default();
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

fn read(reader: &mut BufReader<UnixStream>) -> std::io::Result<serde_json::Value> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "host closed",
        ));
    }
    Ok(serde_json::from_str(&line).unwrap_or(serde_json::Value::Null))
}

/// One frame. Coordinates are the host's normalised scene space: -4..4 on both
/// axes, y up, so the same document draws at any window size.
fn frame(t: f32) -> Scene {
    let x = (t * 0.9).sin() * 2.6;
    let y = (t * 1.4).cos() * 1.4;
    // Scene space is -4..4 on both axes, y up, so a full-bleed node is scale 8.
    // The renderer maps that onto whatever the window is; nothing here knows a
    // pixel size, which is why the same document draws on any display.
    Scene::new(vec![
        Node {
            id: "ground".into(),
            parent: None,
            transform: Transform {
                scale: [8.0, 8.0, 1.0],
                ..Default::default()
            },
            content: Content::Rect(Rect {
                ink: Some(Ink::Background),
                rgb: Some([14, 18, 22]),
                opacity: 1.0,
                radius: 0.0,
            }),
        },
        Node {
            id: "orbit".into(),
            parent: None,
            transform: Transform {
                translation: [x, y, 0.2],
                scale: [0.9, 0.9, 1.0],
                ..Default::default()
            },
            content: Content::Rect(Rect {
                ink: Some(Ink::Primary),
                rgb: Some([110, 168, 254]),
                opacity: 0.9,
                radius: 24.0,
            }),
        },
        Node {
            id: "title".into(),
            parent: None,
            transform: Transform {
                translation: [-3.5, 3.2, 0.3],
                scale: [0.9, 0.9, 1.0],
                ..Default::default()
            },
            content: Content::Text(Text {
                text: format!("Spore-owned surface · {:.1}s", t),
                ink: Some(Ink::Text),
                rgb: None,
                opacity: 1.0,
            }),
        },
        Node {
            id: "hint".into(),
            parent: None,
            transform: Transform {
                translation: [-3.5, -3.4, 0.3],
                scale: [0.7, 0.7, 1.0],
                ..Default::default()
            },
            content: Content::Text(Text {
                text: "This window is drawn by a producer over ui.sock".into(),
                ink: Some(Ink::Muted),
                rgb: None,
                opacity: 1.0,
            }),
        },
    ])
}
