//! Minimal real terminal surface: minifb window + 8x8 font.
//!
//! The Spore remains the VT parser; this module only draws the cell grid and
//! forwards input bytes to the host PTY.

use anyhow::{Result, anyhow};
use font8x8::{BASIC_FONTS, UnicodeFonts};
use minifb::{Key, Window, WindowOptions};
use serde_json::Value;
use wasmtime::Store;

use crate::{State, TerminalSpore, session};

const CELL_W: usize = 8;
const CELL_H: usize = 16;
const SCALE: usize = 2;
const BG: u32 = 0x101014;
const FG: u32 = 0xe6e6e6;

pub fn run(store: &mut Store<State>, bindings: &TerminalSpore, snapshot: &[u8]) -> Result<()> {
    let mut parsed = snapshot_value(snapshot)?;
    let cols = parsed
        .pointer("/terminal/cols")
        .and_then(Value::as_u64)
        .unwrap_or(80) as usize;
    let rows = parsed
        .pointer("/terminal/rows")
        .and_then(Value::as_u64)
        .unwrap_or(24) as usize;
    let width = cols * CELL_W * SCALE;
    let height = rows * CELL_H * SCALE;
    let mut window = Window::new(
        "Soil terminal",
        width,
        height,
        WindowOptions {
            resize: false,
            ..WindowOptions::default()
        },
    )
    .map_err(|error| anyhow!("open terminal window: {error}"))?;
    window.set_target_fps(60);

    let mut buffer = vec![BG; width * height];
    let mut dirty = true;

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let handle = store.data().last_session;
        let mut new_bytes = false;
        if let Some(handle) = handle {
            match session::Host::read(store.data_mut(), handle, 64 * 1024, 10) {
                Ok(Some(bytes)) if !bytes.is_empty() => {
                    bindings
                        .call_feed(&mut *store, &bytes)
                        .map_err(|error| anyhow!("feed: {error:#}"))?
                        .map_err(|message| anyhow!("feed: {message}"))?;
                    new_bytes = true;
                }
                _ => {}
            }
        }
        if new_bytes {
            bindings
                .call_present(&mut *store)
                .map_err(|error| anyhow!("present: {error:#}"))?
                .map_err(|message| anyhow!("present: {message}"))?;
            let snapshot = bindings
                .call_snapshot(&mut *store)
                .map_err(|error| anyhow!("snapshot: {error:#}"))?
                .map_err(|message| anyhow!("snapshot: {message}"))?;
            parsed = snapshot_value(&snapshot)?;
            dirty = true;
        }

        if dirty {
            draw(&parsed, &mut buffer, width, cols, rows);
            window
                .update_with_buffer(&buffer, width, height)
                .map_err(|error| anyhow!("present window: {error}"))?;
            dirty = false;
        }

        if let Some(handle) = store.data().last_session {
            let keys = window.get_keys();
            let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
            let mut input = Vec::new();
            for key in keys {
                if let Some(bytes) = key_bytes(key, ctrl) {
                    input.extend_from_slice(&bytes);
                }
            }
            if !input.is_empty() {
                let _ = session::Host::write(store.data_mut(), handle, input);
            }
        }
    }
    Ok(())
}

fn snapshot_value(snapshot: &[u8]) -> Result<Value> {
    serde_json::from_slice(snapshot).map_err(|error| anyhow!("parse snapshot: {error}"))
}

fn draw(frame: &Value, buffer: &mut [u32], width: usize, cols: usize, rows: usize) {
    buffer.fill(BG);
    let Some(cells) = frame.pointer("/terminal/cells").and_then(Value::as_array) else {
        return;
    };
    for row in 0..rows {
        for col in 0..cols {
            let index = row * cols + col;
            let Some(cell) = cells.get(index) else { break };
            let mut fg = cell.get("fg").and_then(Value::as_u64).unwrap_or(0) as u32;
            let mut bg = cell.get("bg").and_then(Value::as_u64).unwrap_or(0) as u32;
            let flags = cell.get("flags").and_then(Value::as_u64).unwrap_or(0) as u32;
            if fg == 0 {
                fg = FG;
            }
            if bg == 0 {
                bg = BG;
            }
            if flags & 8 != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }
            let width_cells = cell.get("width").and_then(Value::as_u64).unwrap_or(1) as usize;
            if width_cells == 0 {
                continue;
            }
            let text = cell.get("text").and_then(Value::as_str).unwrap_or(" ");
            let ch = text.chars().next().unwrap_or(' ');
            let x0 = col * CELL_W * SCALE;
            let y0 = row * CELL_H * SCALE;
            for y in 0..CELL_H * SCALE {
                for x in 0..CELL_W * SCALE * width_cells {
                    let offset = (y0 + y) * width + x0 + x;
                    if offset < buffer.len() {
                        buffer[offset] = bg;
                    }
                }
            }
            if let Some(glyph) = BASIC_FONTS.get(ch) {
                for (gy, bits) in glyph.iter().enumerate() {
                    for gx in 0..8 {
                        if bits & (1 << gx) != 0 {
                            for sy in 0..SCALE {
                                for sx in 0..SCALE {
                                    let x = x0 + gx * SCALE + sx;
                                    let y = y0 + gy * SCALE + sy;
                                    let offset = y * width + x;
                                    if offset < buffer.len() {
                                        buffer[offset] = fg;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let visible = frame
        .pointer("/terminal/cursor_visible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if visible {
        let row = frame
            .pointer("/terminal/cursor_row")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let col = frame
            .pointer("/terminal/cursor_col")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let x0 = col * CELL_W * SCALE;
        let y0 = row * CELL_H * SCALE;
        for y in 0..CELL_H * SCALE {
            for x in 0..CELL_W * SCALE {
                let offset = (y0 + y) * width + x0 + x;
                if offset < buffer.len() {
                    buffer[offset] ^= 0xffffff;
                }
            }
        }
    }
}

fn key_bytes(key: Key, ctrl: bool) -> Option<Vec<u8>> {
    if ctrl && let Some(byte) = letter(key) {
        return Some(vec![byte & 0x1f]);
    }
    let bytes: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Backspace => b"\x7f",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        Key::Up => b"\x1b[A",
        Key::Down => b"\x1b[B",
        Key::Right => b"\x1b[C",
        Key::Left => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Delete => b"\x1b[3~",
        Key::Space => b" ",
        Key::Key0 => b"0",
        Key::Key1 => b"1",
        Key::Key2 => b"2",
        Key::Key3 => b"3",
        Key::Key4 => b"4",
        Key::Key5 => b"5",
        Key::Key6 => b"6",
        Key::Key7 => b"7",
        Key::Key8 => b"8",
        Key::Key9 => b"9",
        Key::A => b"a",
        Key::B => b"b",
        Key::C => b"c",
        Key::D => b"d",
        Key::E => b"e",
        Key::F => b"f",
        Key::G => b"g",
        Key::H => b"h",
        Key::I => b"i",
        Key::J => b"j",
        Key::K => b"k",
        Key::L => b"l",
        Key::M => b"m",
        Key::N => b"n",
        Key::O => b"o",
        Key::P => b"p",
        Key::Q => b"q",
        Key::R => b"r",
        Key::S => b"s",
        Key::T => b"t",
        Key::U => b"u",
        Key::V => b"v",
        Key::W => b"w",
        Key::X => b"x",
        Key::Y => b"y",
        Key::Z => b"z",
        Key::Minus => b"-",
        Key::Equal => b"=",
        Key::LeftBracket => b"[",
        Key::RightBracket => b"]",
        Key::Backslash => b"\\",
        Key::Semicolon => b";",
        Key::Apostrophe => b"'",
        Key::Comma => b",",
        Key::Period => b".",
        Key::Slash => b"/",
        Key::Backquote => b"`",
        _ => return None,
    };
    Some(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_letters_become_control_bytes() {
        assert_eq!(key_bytes(Key::C, true), Some(vec![0x03]));
        assert_eq!(key_bytes(Key::A, true), Some(vec![0x01]));
        assert_eq!(key_bytes(Key::C, false), Some(b"c".to_vec()));
    }

    #[test]
    fn named_keys_emit_terminal_sequences() {
        assert_eq!(key_bytes(Key::Enter, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Key::Up, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(Key::F1, false), None);
    }
}

fn letter(key: Key) -> Option<u8> {
    Some(match key {
        Key::A => b'a',
        Key::B => b'b',
        Key::C => b'c',
        Key::D => b'd',
        Key::E => b'e',
        Key::F => b'f',
        Key::G => b'g',
        Key::H => b'h',
        Key::I => b'i',
        Key::J => b'j',
        Key::K => b'k',
        Key::L => b'l',
        Key::M => b'm',
        Key::N => b'n',
        Key::O => b'o',
        Key::P => b'p',
        Key::Q => b'q',
        Key::R => b'r',
        Key::S => b's',
        Key::T => b't',
        Key::U => b'u',
        Key::V => b'v',
        Key::W => b'w',
        Key::X => b'x',
        Key::Y => b'y',
        Key::Z => b'z',
        _ => return None,
    })
}
