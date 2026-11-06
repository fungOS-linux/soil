//! Host-neutral input for Soil.
//!
//! Canvas, canvas-web and native winit hosts all produce the same bounded event
//! shape. Platform keycodes and window handles stay in the adapter; a producer
//! sees normalized coordinates, semantic keys, modifiers and text.
//!
//! The type is serializable because the Canvas producer ABI is JSON. That does
//! not make JSON the internal model: hosts can route these values directly.

use serde::Serialize as DeriveSerialize;
use serde::ser::{Serialize, Serializer};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, DeriveSerialize)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Modifiers {
    pub fn any(self) -> bool {
        self.shift || self.control || self.alt || self.meta
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    Enter,
    Press,
    Move,
    Release,
    Leave,
    Cancel,
}

impl PointerPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Press => "press",
            Self::Move => "move",
            Self::Release => "release",
            Self::Leave => "leave",
            Self::Cancel => "cancel",
        }
    }
}

impl Serialize for PointerPhase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, DeriveSerialize)]
#[serde(rename_all = "lowercase")]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
}

#[derive(Clone, Debug, PartialEq, DeriveSerialize)]
pub struct PointerEvent {
    pub phase: PointerPhase,
    /// Surface-local normalized X in `0..=1`.
    pub x: f32,
    /// Surface-local normalized Y in `0..=1`.
    pub y: f32,
    pub pointer_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<PointerButton>,
    #[serde(flatten)]
    pub modifiers: Modifiers,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, DeriveSerialize)]
pub struct ScrollEvent {
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
    #[serde(flatten)]
    pub modifiers: Modifiers,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Backspace,
    Tab,
    Escape,
    Space,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
    Other(String),
}

impl NamedKey {
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "enter" | "return" => Self::Enter,
            "backspace" => Self::Backspace,
            "tab" => Self::Tab,
            "escape" | "esc" => Self::Escape,
            "space" => Self::Space,
            "arrowup" | "up" => Self::ArrowUp,
            "arrowdown" | "down" => Self::ArrowDown,
            "arrowleft" | "left" => Self::ArrowLeft,
            "arrowright" | "right" => Self::ArrowRight,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" => Self::PageUp,
            "pagedown" => Self::PageDown,
            "insert" => Self::Insert,
            "delete" | "del" => Self::Delete,
            other if other.starts_with('f') => other[1..]
                .parse()
                .ok()
                .filter(|number| (1..=24).contains(number))
                .map(Self::F)
                .unwrap_or_else(|| Self::Other(other.to_owned())),
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            Self::Enter => "enter".into(),
            Self::Backspace => "backspace".into(),
            Self::Tab => "tab".into(),
            Self::Escape => "escape".into(),
            Self::Space => "space".into(),
            Self::ArrowUp => "arrowup".into(),
            Self::ArrowDown => "arrowdown".into(),
            Self::ArrowLeft => "arrowleft".into(),
            Self::ArrowRight => "arrowright".into(),
            Self::Home => "home".into(),
            Self::End => "end".into(),
            Self::PageUp => "pageup".into(),
            Self::PageDown => "pagedown".into(),
            Self::Insert => "insert".into(),
            Self::Delete => "delete".into(),
            Self::F(number) => format!("f{number}"),
            Self::Other(name) => name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Character(String),
    Named(NamedKey),
}

impl Key {
    pub fn named(name: &str) -> Self {
        Self::Named(NamedKey::from_name(name))
    }

    pub fn character(text: impl Into<String>) -> Self {
        Self::Character(text.into())
    }

    pub fn as_str(&self) -> String {
        match self {
            Self::Character(text) => text.clone(),
            Self::Named(key) => key.as_str(),
        }
    }
}

impl Serialize for Key {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveSerialize)]
pub struct KeyEvent {
    pub key: Key,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub repeat: bool,
    #[serde(flatten)]
    pub modifiers: Modifiers,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveSerialize)]
pub struct FocusEvent {
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImePhase {
    Preedit,
    Commit,
}

impl Serialize for ImePhase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Preedit => "preedit",
            Self::Commit => "commit",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveSerialize)]
pub struct ImeEvent {
    pub phase: ImePhase,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

/// The normalized event envelope. `kind` is per-event, not a wrapper field, so
/// serialization matches the existing producer protocol.
#[derive(Clone, Debug, PartialEq, DeriveSerialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum InputEvent {
    Pointer(PointerEvent),
    Scroll(ScrollEvent),
    Key(KeyEvent),
    Focus(FocusEvent),
    Ime(ImeEvent),
}

impl InputEvent {
    pub fn at_ms(&self) -> Option<u64> {
        match self {
            Self::Pointer(event) => event.at_ms,
            Self::Scroll(event) => event.at_ms,
            Self::Key(event) => event.at_ms,
            Self::Focus(event) => event.at_ms,
            Self::Ime(event) => event.at_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn clamp_unit(self) -> Self {
        Self {
            x: self.x.clamp(0.0, 1.0),
            y: self.y.clamp(0.0, 1.0),
        }
    }
}

/// Encode a terminal key to the bytes an xterm-compatible PTY expects.
///
/// This is deliberately in `soil-input`: native terminal windows and a future
/// WebSocket terminal should not each invent their own arrow-key table.
pub fn terminal_bytes(key: &Key, text: Option<&str>, modifiers: Modifiers) -> Option<Vec<u8>> {
    if modifiers.meta {
        return None;
    }
    let alt = modifiers.alt;
    if let Key::Character(value) = key {
        if modifiers.control {
            let ch = value.chars().next()?;
            let upper = ch.to_ascii_uppercase();
            if upper.is_ascii() {
                let byte = upper as u8;
                if (b'@'..=b'_').contains(&byte) {
                    return Some(vec![byte & 0x1f]);
                }
            }
            return None;
        }
        let mut bytes = text.unwrap_or(value).as_bytes().to_vec();
        if alt {
            bytes.insert(0, 0x1b);
        }
        return Some(bytes);
    }
    let Key::Named(named) = key else { return None };
    let sequence: &[u8] = match named {
        NamedKey::Enter => b"\r",
        NamedKey::Backspace => b"\x7f",
        NamedKey::Tab => b"\t",
        NamedKey::Escape => b"\x1b",
        NamedKey::Space => b" ",
        NamedKey::ArrowUp => b"\x1b[A",
        NamedKey::ArrowDown => b"\x1b[B",
        NamedKey::ArrowRight => b"\x1b[C",
        NamedKey::ArrowLeft => b"\x1b[D",
        NamedKey::Home => b"\x1b[H",
        NamedKey::End => b"\x1b[F",
        NamedKey::Insert => b"\x1b[2~",
        NamedKey::Delete => b"\x1b[3~",
        NamedKey::PageUp => b"\x1b[5~",
        NamedKey::PageDown => b"\x1b[6~",
        NamedKey::F(1) => b"\x1bOP",
        NamedKey::F(2) => b"\x1bOQ",
        NamedKey::F(3) => b"\x1bOR",
        NamedKey::F(4) => b"\x1bOS",
        NamedKey::F(5) => b"\x1b[15~",
        NamedKey::F(6) => b"\x1b[17~",
        NamedKey::F(7) => b"\x1b[18~",
        NamedKey::F(8) => b"\x1b[19~",
        NamedKey::F(9) => b"\x1b[20~",
        NamedKey::F(10) => b"\x1b[21~",
        NamedKey::F(11) => b"\x1b[23~",
        NamedKey::F(12) => b"\x1b[24~",
        NamedKey::Other(_) | NamedKey::F(_) => return None,
    };
    let mut bytes = sequence.to_vec();
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(feature = "winit")]
mod winit_adapter {
    use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey as WinitNamed};

    use super::{Key, Modifiers, NamedKey};

    impl From<ModifiersState> for Modifiers {
        fn from(value: ModifiersState) -> Self {
            Self {
                shift: value.shift_key(),
                control: value.control_key(),
                alt: value.alt_key(),
                meta: value.super_key(),
            }
        }
    }

    impl Key {
        pub fn from_winit(value: &WinitKey) -> Option<Self> {
            match value {
                WinitKey::Character(text) => Some(Self::Character(text.to_string())),
                WinitKey::Named(named) => Some(Self::Named(match named {
                    WinitNamed::Enter => NamedKey::Enter,
                    WinitNamed::Backspace => NamedKey::Backspace,
                    WinitNamed::Tab => NamedKey::Tab,
                    WinitNamed::Escape => NamedKey::Escape,
                    WinitNamed::Space => NamedKey::Space,
                    WinitNamed::ArrowUp => NamedKey::ArrowUp,
                    WinitNamed::ArrowDown => NamedKey::ArrowDown,
                    WinitNamed::ArrowLeft => NamedKey::ArrowLeft,
                    WinitNamed::ArrowRight => NamedKey::ArrowRight,
                    WinitNamed::Home => NamedKey::Home,
                    WinitNamed::End => NamedKey::End,
                    WinitNamed::PageUp => NamedKey::PageUp,
                    WinitNamed::PageDown => NamedKey::PageDown,
                    WinitNamed::Insert => NamedKey::Insert,
                    WinitNamed::Delete => NamedKey::Delete,
                    WinitNamed::F1 => NamedKey::F(1),
                    WinitNamed::F2 => NamedKey::F(2),
                    WinitNamed::F3 => NamedKey::F(3),
                    WinitNamed::F4 => NamedKey::F(4),
                    WinitNamed::F5 => NamedKey::F(5),
                    WinitNamed::F6 => NamedKey::F(6),
                    WinitNamed::F7 => NamedKey::F(7),
                    WinitNamed::F8 => NamedKey::F(8),
                    WinitNamed::F9 => NamedKey::F(9),
                    WinitNamed::F10 => NamedKey::F(10),
                    WinitNamed::F11 => NamedKey::F(11),
                    WinitNamed::F12 => NamedKey::F(12),
                    other => NamedKey::Other(format!("{other:?}").to_ascii_lowercase()),
                })),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_serializes_to_the_producer_protocol() {
        let event = InputEvent::Pointer(PointerEvent {
            phase: PointerPhase::Press,
            x: 0.25,
            y: 0.75,
            pointer_id: 1,
            button: Some(PointerButton::Primary),
            modifiers: Modifiers {
                shift: true,
                ..Default::default()
            },
            at_ms: Some(42),
        });
        let value = serde_json::to_value(event).expect("json");
        assert_eq!(value["kind"], "pointer");
        assert_eq!(value["phase"], "press");
        assert_eq!(value["shift"], true);
        assert_eq!(value["at_ms"], 42);
    }

    #[test]
    fn terminal_keys_are_shared_across_hosts() {
        let up = terminal_bytes(&Key::Named(NamedKey::ArrowUp), None, Modifiers::default())
            .expect("arrow");
        assert_eq!(up, b"\x1b[A");
        let ctrl_c = terminal_bytes(
            &Key::Character("c".into()),
            Some("c"),
            Modifiers {
                control: true,
                ..Default::default()
            },
        )
        .expect("ctrl-c");
        assert_eq!(ctrl_c, vec![3]);
    }
}
