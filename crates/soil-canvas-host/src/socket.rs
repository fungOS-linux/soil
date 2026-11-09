//! Canvas producer-socket presenter.
//!
//! Canvas already owns the rasterizer and the window. This presenter gives a
//! Spore the same `soil:canvas` capability without teaching it the socket path,
//! the producer protocol, or the frame format. The host opens the connection,
//! sends `hello`, and forwards each validated `soil-scene` document as a
//! `present` message.
//!
//! The protocol is line-delimited JSON on `<Canvas runtime>/ui.sock`. It is
//! deliberately not a Rust ABI: the document is the versioned boundary.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::{Presenter, PresenterError};

/// A `soil:canvas` presenter backed by Canvas's producer socket.
pub struct CanvasSocket {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    surface_id: String,
    generation: u64,
    pending: VecDeque<serde_json::Value>,
}

impl CanvasSocket {
    /// Open the producer connection and wait for Canvas to accept the surface.
    pub fn connect(
        socket: impl AsRef<Path>,
        surface_id: &str,
        title: &str,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Self> {
        let writer = UnixStream::connect(socket.as_ref())?;
        writer.set_write_timeout(Some(Duration::from_secs(2)))?;
        let reader_stream = writer.try_clone()?;
        reader_stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut socket = Self {
            writer,
            reader: BufReader::new(reader_stream),
            surface_id: surface_id.to_owned(),
            generation: 0,
            pending: VecDeque::new(),
        };
        let hello = serde_json::json!({
            "kind": "hello",
            "surface_id": surface_id,
            "title": title,
            "width": width,
            "height": height,
        });
        socket.send(&hello)?;
        socket.await_reply()?;
        Ok(socket)
    }

    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Input Canvas delivered while the host was waiting for a present reply.
    pub fn take_input(&mut self) -> Vec<serde_json::Value> {
        self.pending.drain(..).collect()
    }

    fn send(&mut self, message: &serde_json::Value) -> Result<(), PresenterError> {
        let mut line = serde_json::to_vec(message).map_err(|_| PresenterError::Invalid)?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .map_err(|_| PresenterError::Backend)?;
        self.writer.flush().map_err(|_| PresenterError::Backend)
    }

    /// Read until Canvas returns a state or an error. Pointer/scroll/key lines
    /// are producer input and are retained for the runner, not treated as a
    /// present acknowledgement.
    fn await_reply(&mut self) -> Result<(), PresenterError> {
        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|_| PresenterError::Backend)?;
            if read == 0 {
                return Err(PresenterError::Backend);
            }
            let message: serde_json::Value =
                serde_json::from_str(&line).map_err(|_| PresenterError::Invalid)?;
            match message.get("kind").and_then(|kind| kind.as_str()) {
                Some("state") => {
                    if message.get("message").and_then(|value| value.as_str()) != Some("ok") {
                        return Err(PresenterError::Invalid);
                    }
                    if let Some(generation) = message.get("generation").and_then(|v| v.as_u64()) {
                        self.generation = self.generation.max(generation);
                    }
                    return Ok(());
                }
                Some("error") => return Err(PresenterError::Invalid),
                Some("pointer" | "scroll" | "key") if self.pending.len() < 64 => {
                    self.pending.push_back(message);
                }
                Some("pointer" | "scroll" | "key") => {}
                _ => {}
            }
        }
    }
}

impl Presenter for CanvasSocket {
    fn present(&mut self, present: &soil_scene::Present) -> Result<u64, PresenterError> {
        let message = serde_json::json!({
            "kind": "present",
            "payload": present,
        });
        self.send(&message)?;
        self.await_reply()?;
        Ok(self.generation)
    }

    fn close(&mut self) -> Result<(), PresenterError> {
        self.send(&serde_json::json!({ "kind": "close" }))?;
        self.await_reply()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    use soil_scene::{Content, Node, Rect, Scene, Transform};

    use super::*;

    fn scene() -> soil_scene::Present {
        soil_scene::Present::Scene(Scene::new(vec![Node {
            id: "panel".into(),
            parent: None,
            transform: Transform {
                scale: [100.0, 40.0, 1.0],
                ..Transform::default()
            },
            content: Content::Rect(Rect {
                ink: Some(soil_scene::Ink::Surface),
                ..Rect::default()
            }),
            transition_ms: None,
        }]))
    }

    #[test]
    fn forwards_a_scene_document_and_tracks_canvas_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);

            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let hello: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(hello["kind"], "hello");
            assert_eq!(hello["surface_id"], "sdk-panel");
            assert_eq!(hello["width"], 1200);
            assert_eq!(hello["height"], 840);
            writeln!(
                writer,
                r#"{{"kind":"state","message":"ok","generation":1}}"#
            )
            .unwrap();

            line.clear();
            reader.read_line(&mut line).unwrap();
            let present: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(present["kind"], "present");
            assert_eq!(present["payload"]["kind"], "scene");
            assert_eq!(present["payload"]["nodes"][0]["id"], "panel");
            writeln!(
                writer,
                r#"{{"kind":"state","message":"ok","generation":2}}"#
            )
            .unwrap();

            line.clear();
            reader.read_line(&mut line).unwrap();
            let close: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(close["kind"], "close");
            writeln!(
                writer,
                r#"{{"kind":"state","message":"ok","generation":2}}"#
            )
            .unwrap();
        });

        let mut present_path = CanvasSocket::connect(&path, "sdk-panel", "SDK panel", 1200, 840)
            .expect("Canvas socket");
        assert_eq!(present_path.generation(), 1);
        assert_eq!(present_path.present(&scene()).unwrap(), 2);
        assert_eq!(present_path.generation(), 2);
        present_path.close().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn an_error_reply_is_a_named_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            writeln!(
                writer,
                r#"{{"kind":"state","message":"ok","generation":1}}"#
            )
            .unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            writeln!(
                writer,
                r#"{{"kind":"error","message":"scene has 4097 nodes","generation":1}}"#
            )
            .unwrap();
        });

        let mut socket = CanvasSocket::connect(&path, "sdk-panel", "SDK panel", 1200, 840).unwrap();
        assert_eq!(
            socket.present(&scene()).unwrap_err(),
            PresenterError::Invalid
        );
        server.join().unwrap();
    }
}
