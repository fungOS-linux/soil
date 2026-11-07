//! A producer, shaped like a Spore, that presents into a Canvas window.
//!
//! It talks the `ui.sock` protocol: hello, then a payload from `soil-scene` on
//! every frame it wants drawn. No compositor code is linked, no native build is
//! needed to change what it draws -- which is the point of the payload being
//! data rather than an ABI.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use soil_scene::{Content, Ink, Node, Present, Rect, Scene, Shader, Text, Transform};

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
    // The host owns the clock: it asks for a frame when it can draw one and says
    // how long it has been. A producer that renders on its own timer can only
    // guess, and guessing wrong is either a queue of stale frames or a stutter.
    let mut now_ms: u64 = 0;
    loop {
        let Some(clock) = wait_for_frame(&mut reader, &started) else {
            return Ok(());
        };
        now_ms = clock.0;
        let t = now_ms as f32 / 1000.0;
        let _dt = clock.1;
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

/// Block until the host asks for a frame, returning `(now_ms, dt_ms)`.
fn wait_for_frame(reader: &mut BufReader<UnixStream>, _started: &Instant) -> Option<(u64, u64)> {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let Ok(reply) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                match reply.get("kind").and_then(|k| k.as_str()) {
                    Some("frame") => {
                        let now = reply.get("now_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                        let dt = reply.get("dt_ms").and_then(|v| v.as_u64()).unwrap_or(16);
                        return Some((now.saturating_sub(start_ms()), dt));
                    }
                    Some("error") => {
                        eprintln!(
                            "present refused: {}",
                            reply.get("message").and_then(|m| m.as_str()).unwrap_or("?")
                        );
                    }
                    _ => {}
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return None,
        }
    }
}

/// The producer's zero: the same wall clock the host stamps frames with.
fn start_ms() -> u64 {
    static START: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *START.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
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

/// A fragment-only WGSL component: the host supplies the vertex stage, the
/// extent, and the scale factor in `resolution`, so this only has to say what a
/// pixel looks like.
const PLASMA: &str = r#"
// The host declares Uniforms and binds it at group 0; a producer writes the
// fragment stage and nothing else, so the two cannot disagree about the layout.
@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
  let t = u.resolution.z;
  var p = (uv - 0.5) * vec2<f32>(1.7, 1.0);
  var v = 0.0;
  var q = p;
  for (var i = 0u; i < 8u; i = i + 1u) {
    let f = f32(i);
    q = p + vec2<f32>(cos(t * 0.3 + f * 0.7), sin(t * 0.24 + f * 0.53)) * 0.32;
    v = v + sin(q.x * 3.1 + t) * cos(q.y * 3.7 - t * 0.7) / (f + 1.0);
  }
  let glow = smoothstep(0.9, 0.0, length(p));
  let base = vec3<f32>(0.06, 0.09, 0.14);
  let accent = vec3<f32>(0.43, 0.66, 1.0) * (0.5 + 0.5 * cos(v * 2.0 + t * 0.4));
  return vec4<f32>(base + accent * glow * 0.85, 1.0);
}
"#;

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
            transition_ms: None,
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
            transition_ms: None,
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
        // A GPU component, written here as data: the host compiles the WGSL,
        // caches the pipeline by digest, and draws it in this node's rectangle.
        // Changing it needs no compositor build, which is the whole point.
        Node {
            id: "plasma".into(),
            parent: None,
            transition_ms: None,
            // Scene placement is a top-left corner, y up: a 7.2x4.4 node centred
            // in an 8x8 space starts at (-3.6, 2.2).
            transform: Transform {
                translation: [-3.6, 2.2, 0.1],
                scale: [7.2, 4.4, 1.0],
                ..Default::default()
            },
            content: Content::Shader(Shader {
                wgsl: PLASMA.into(),
                // `t` first: the host puts it in resolution.z for convenience,
                // and the rest land in p0..p4 in order.
                uniforms: vec![t],
            }),
        },
        Node {
            id: "title".into(),
            parent: None,
            transition_ms: None,
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
            transition_ms: None,
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
