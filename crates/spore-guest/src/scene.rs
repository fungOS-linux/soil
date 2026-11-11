//! The scene document: what a view emits, in the order the host composites it.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Rect { x: f32, y: f32, w: f32, h: f32, r: f32, colour: String },
    Text { value: String, x: f32, y: f32, size: f32, colour: String, anchor: String, align: String, font: String, wrap: Option<f32> },
    Hit { id: String, x: f32, y: f32, w: f32, h: f32, action: String },
    /// A scope boundary. Commands between two markers share an identity, which
    /// is what a fragment cache slices on.
    Group(String),
    Clip { x: f32, y: f32, w: f32, h: f32 },
    ClipEnd,
}

/// A normalized input event, matching `soil:input`.
#[derive(Clone, Debug)]
pub enum Input {
    PointerMove(f32, f32),
    PointerButton { pressed: bool, x: f32, y: f32 },
    Key { name: String, pressed: bool },
}

fn number(value: &Value, index: usize) -> f32 {
    value.get(index).and_then(Value::as_f64).unwrap_or(0.0) as f32
}

fn string(value: &Value, index: usize) -> String {
    value.get(index).and_then(Value::as_str).unwrap_or_default().to_string()
}

impl Command {
    pub fn from_json(value: &Value) -> Option<Self> {
        let kind = value.get(0)?.as_str()?;
        Some(match kind {
            "rect" => Command::Rect {
                x: number(value, 1), y: number(value, 2), w: number(value, 3), h: number(value, 4),
                r: number(value, 5), colour: string(value, 6),
            },
            "text" => Command::Text {
                value: string(value, 1), x: number(value, 2), y: number(value, 3), size: number(value, 4),
                colour: string(value, 5), anchor: string(value, 6), align: string(value, 7),
                font: string(value, 8), wrap: value.get(9).and_then(Value::as_f64).map(|n| n as f32),
            },
            "hit" => Command::Hit {
                id: string(value, 1), x: number(value, 2), y: number(value, 3),
                w: number(value, 4), h: number(value, 5), action: string(value, 6),
            },
            "group" => Command::Group(string(value, 1)),
            "clip" => Command::Clip { x: number(value, 1), y: number(value, 2), w: number(value, 3), h: number(value, 4) },
            "clip-end" => Command::ClipEnd,
            _ => return None,
        })
    }

    /// The key this command belongs to, when the view scoped it.
    pub fn group(&self) -> Option<&str> {
        match self {
            Command::Group(key) => Some(key),
            _ => None,
        }
    }
}
