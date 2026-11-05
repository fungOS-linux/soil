//! Key events → the bytes a terminal program expects.
//!
//! Small and explicit rather than clever: a terminal's input contract is a
//! handful of escape sequences, and a program that enabled the kitty keyboard
//! protocol will get its own encoding from `libghostty-vt` later. Until the
//! guest can negotiate that, this is the portable subset every TUI understands.

use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Takes a logical key rather than a whole `KeyEvent`: the encoding depends on
/// the key and the modifiers and on nothing else, and a portable signature is
/// also a testable one.
pub fn key(logical_key: &Key, modifiers: ModifiersState) -> Vec<u8> {
    let ctrl = modifiers.contains(ModifiersState::CONTROL);
    let alt = modifiers.contains(ModifiersState::ALT);

    if let Key::Named(named) = logical_key {
        let bytes: &[u8] = match named {
            NamedKey::Enter => b"\r",
            NamedKey::Tab => b"\t",
            NamedKey::Backspace => &[0x7f],
            NamedKey::Escape => &[0x1b],
            NamedKey::ArrowUp => b"\x1b[A",
            NamedKey::ArrowDown => b"\x1b[B",
            NamedKey::ArrowRight => b"\x1b[C",
            NamedKey::ArrowLeft => b"\x1b[D",
            NamedKey::Home => b"\x1b[H",
            NamedKey::End => b"\x1b[F",
            NamedKey::PageUp => b"\x1b[5~",
            NamedKey::PageDown => b"\x1b[6~",
            NamedKey::Delete => b"\x1b[3~",
            NamedKey::Space => b" ",
            _ => b"",
        };
        return with_alt(bytes.to_vec(), alt);
    }

    let Key::Character(text) = logical_key else {
        return Vec::new();
    };
    // Ctrl-A..Ctrl-Z map onto 0x01..0x1a, which is still how every shell and
    // editor receives them.
    if ctrl
        && let Some(ch) = text.chars().next()
    {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() {
            return vec![(lower as u8) & 0x1f];
        }
    }
    with_alt(text.as_bytes().to_vec(), alt)
}

fn with_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt && !bytes.is_empty() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    fn character(text: &str) -> Key {
        Key::Character(SmolStr::new(text))
    }

    #[test]
    fn plain_characters_pass_through() {
        assert_eq!(key(&character("a"), ModifiersState::empty()), b"a");
    }

    #[test]
    fn control_keys_become_control_codes() {
        assert_eq!(
            key(&character("c"), ModifiersState::CONTROL),
            vec![0x03],
            "Ctrl-C is ETX"
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(key(&character("f"), ModifiersState::ALT), b"\x1bf");
    }

    #[test]
    fn named_keys_use_the_standard_sequences() {
        let named = |key| Key::Named(key);
        assert_eq!(key(&named(NamedKey::Enter), ModifiersState::empty()), b"\r");
        assert_eq!(
            key(&named(NamedKey::ArrowUp), ModifiersState::empty()),
            b"\x1b[A"
        );
        assert_eq!(
            key(&named(NamedKey::Backspace), ModifiersState::empty()),
            vec![0x7f]
        );
    }
}
