//! Fixture Spore for `soil:surface-import`.
//!
//! It opens a host-owned surface, drains metadata/damage events, forwards one
//! pointer event, and closes. It never receives pixels or a native handle.

wit_bindgen::generate!({ path: "wit", world: "surface-probe", generate_all });

use soil::surface_import::surface::{self, Button, Event, Input, Point, PointerButton};

struct Component;

export!(Component);

impl Guest for Component {
    fn run(_args: Vec<String>) -> Result<(), String> {
        let handle = surface::open("fixture").map_err(debug)?;
        let info = handle.info().map_err(debug)?;
        if info.width != 640 || info.height != 480 {
            return Err("unexpected surface info".into());
        }
        match handle.next(100).map_err(debug)? {
            Some(Event::Resized(info)) => {
                if info.kind != surface::Kind::Native {
                    return Err("expected native surface".into());
                }
            }
            other => return Err(format!("expected resize, got {other:?}")),
        }
        match handle.next(100).map_err(debug)? {
            Some(Event::Frame(frame)) => {
                if frame.id != 7 || !frame.keyframe || frame.damage.len() != 1 {
                    return Err("unexpected frame metadata".into());
                }
            }
            other => return Err(format!("expected frame, got {other:?}")),
        }
        handle
            .input(&Input::PointerButton(PointerButton {
                button: Button::Primary,
                pressed: true,
                point: Point { x: 0.25, y: 0.75 },
            }))
            .map_err(debug)?;
        handle.input(&Input::Text("a".into())).map_err(debug)?;
        handle.close().map_err(debug)?;
        Ok(())
    }
}

fn debug(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
