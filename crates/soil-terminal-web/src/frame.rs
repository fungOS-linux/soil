//! Snapshot → browser frames.
//!
//! A frame is a diff against the previous snapshot: only rows that changed are
//! sent, and each row is a list of style runs rather than one object per cell.
//! The first frame (or a resize) is full, so a reconnecting browser can rebuild
//! the screen from one message.

use serde_json::{Value, json};
use soil_vt::{Cell, Snapshot};

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const UNDERLINE: u8 = 4;
const FAINT: u8 = 8;
const INVERSE: u8 = 16;

pub fn frame(snapshot: &Snapshot, previous: Option<&Snapshot>) -> Value {
    let full = previous.is_none_or(|last| last.cols != snapshot.cols || last.rows != snapshot.rows);
    let mut rows = Vec::new();
    for y in 0..snapshot.rows {
        let runs = row_runs(snapshot, y);
        let changed = full || previous.is_some_and(|last| row_runs(last, y) != runs);
        if changed {
            rows.push(json!({ "y": y, "runs": runs }));
        }
    }
    json!({
        "type": "frame",
        "full": full,
        "cols": snapshot.cols,
        "rows": snapshot.rows,
        "cursor": snapshot.cursor,
        "title": snapshot.title,
        "scroll": snapshot.scroll.map(|scroll| json!({
            "offset": scroll.offset,
            "total": scroll.total,
            "len": scroll.len,
        })),
        "rows": rows,
    })
}

/// One row as `[text, fg, bg, flags]` runs. A wide glyph's spacer is skipped:
/// the leading cell carries the cluster and a monospace font gives it two cells.
fn row_runs(snapshot: &Snapshot, y: u16) -> Vec<Value> {
    let mut runs: Vec<Run> = Vec::new();
    for x in 0..snapshot.cols {
        let Some(cell) = snapshot.cell(x, y) else {
            break;
        };
        if cell.wide_tail {
            continue;
        }
        let text = if cell.text.is_empty() {
            " ".to_string()
        } else {
            cell.text.clone()
        };
        let fg = cell.fg.map(hex);
        let bg = cell.bg.map(hex);
        let flags = flags(cell);
        match runs.last_mut() {
            Some(last) if last.fg == fg && last.bg == bg && last.flags == flags => {
                last.text.push_str(&text);
            }
            _ => runs.push(Run {
                text,
                fg,
                bg,
                flags,
            }),
        }
    }
    runs.into_iter()
        .map(|run| json!([run.text, run.fg, run.bg, run.flags]))
        .collect()
}

struct Run {
    text: String,
    fg: Option<String>,
    bg: Option<String>,
    flags: u8,
}

/// One full screen as a Lua table literal.
///
/// A scene renderer takes a snapshot, not a diff: it repaints the whole grid for
/// every frame, so there is nothing to merge and no previous frame to hold. The
/// shape is the frame's, with `runs` as `{ text, fg, bg, flags }` arrays.
pub fn lua(snapshot: &Snapshot, extra: &str) -> String {
    let mut out = String::from("return {\n");
    out.push_str(extra);
    out.push_str(&format!(
        "  cols = {}, rows = {},\n  title = {},\n",
        snapshot.cols,
        snapshot.rows,
        lua_string(&snapshot.title)
    ));
    if let Some((x, y)) = snapshot.cursor {
        out.push_str(&format!("  cursor = {{ x = {x}, y = {y}, visible = true }},\n"));
    }
    if let Some(scroll) = snapshot.scroll {
        out.push_str(&format!(
            "  scroll = {{ offset = {}, total = {}, len = {} }},\n",
            scroll.offset, scroll.total, scroll.len
        ));
    }
    // `rows` is the count and `lines` is the array: a table with one key holding
    // both would silently keep whichever came last, and a view reading `rows` as
    // a number would get the list (or nil) and draw the wrong grid.
    out.push_str("  lines = {\n");
    for y in 0..snapshot.rows {
        let mut runs: Vec<String> = Vec::new();
        let mut current: Option<(String, Option<String>, Option<String>, u8)> = None;
        let flush = |run: Option<(String, Option<String>, Option<String>, u8)>, runs: &mut Vec<String>| {
            if let Some((text, fg, bg, flags)) = run {
                runs.push(format!(
                    "{{ {}, {}, {}, {} }}",
                    lua_string(&text),
                    fg.map_or("nil".to_string(), |value| lua_string(&value)),
                    bg.map_or("nil".to_string(), |value| lua_string(&value)),
                    flags
                ));
            }
        };
        for x in 0..snapshot.cols {
            let Some(cell) = snapshot.cell(x, y) else { break };
            if cell.wide_tail {
                continue;
            }
            let text = if cell.text.is_empty() { " ".to_string() } else { cell.text.clone() };
            let fg = cell.fg.map(hex);
            let bg = cell.bg.map(hex);
            let cell_flags = flags(cell);
            match &mut current {
                Some((run_text, run_fg, run_bg, run_flags))
                    if *run_fg == fg && *run_bg == bg && *run_flags == cell_flags =>
                {
                    run_text.push_str(&text);
                }
                _ => {
                    flush(current.take(), &mut runs);
                    current = Some((text, fg, bg, cell_flags));
                }
            }
        }
        flush(current.take(), &mut runs);
        out.push_str(&format!("    {{ y = {y}, runs = {{ {} }} }},\n", runs.join(", ")));
    }
    out.push_str("  },\n}\n");
    out
}

fn lua_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn hex(color: soil_vt::Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

fn flags(cell: &Cell) -> u8 {
    let mut flags = 0;
    if cell.bold {
        flags |= BOLD;
    }
    if cell.italic {
        flags |= ITALIC;
    }
    if cell.underline {
        flags |= UNDERLINE;
    }
    if cell.faint {
        flags |= FAINT;
    }
    if cell.inverse {
        flags |= INVERSE;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(rows: &[&str], cols: u16) -> Snapshot {
        let mut session = soil_vt::Session::new(cols, rows.len() as u16, 10).unwrap();
        session.write(rows.join("\r\n").as_bytes());
        session.snapshot().unwrap()
    }

    #[test]
    fn the_lua_snapshot_is_a_loadable_table() {
        let mut session = soil_vt::Session::new(12, 2, 10).unwrap();
        session.write(b"\x1b[1;31mred\x1b[0m plain");
        let text = lua(&session.snapshot().unwrap(), "");
        assert!(text.starts_with("return {"));
        assert!(text.contains("cols = 12"));
        assert!(text.contains("{ \"red\", \"#"));
        assert!(text.contains("runs = {"));
        // The count and the array must not share a key: `rows = 53` followed by
        // `rows = { ... }` leaves the view reading a table where it expects a
        // number, and it draws the wrong grid instead of failing.
        assert!(text.contains("rows = 2,"), "the row count is not published");
        assert!(text.contains("lines = {"), "the lines are not published");
    }

    #[test]
    fn a_full_frame_carries_every_row() {
        let snapshot = snapshot(&["hello", "world"], 10);
        let frame = frame(&snapshot, None);
        assert_eq!(frame["full"], true);
        assert_eq!(frame["rows"].as_array().unwrap().len(), 2);
        assert_eq!(frame["rows"][0]["runs"][0][0], "hello     ");
    }

    #[test]
    fn a_diff_only_carries_the_changed_row() {
        let first = snapshot(&["hello", "world"], 10);
        let mut session = soil_vt::Session::new(10, 2, 10).unwrap();
        session.write(b"hello\r\nworld");
        let _ = session.snapshot().unwrap();
        session.write(b"\x1b[2;1Hagain");
        let second = session.snapshot().unwrap();
        let frame = frame(&second, Some(&first));
        assert_eq!(frame["full"], false);
        let rows = frame["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["y"], 1);
        assert_eq!(rows[0]["runs"][0][0], "again     ");
    }

    #[test]
    fn a_resize_forces_a_full_frame() {
        let first = snapshot(&["hello"], 10);
        let mut session = soil_vt::Session::new(4, 1, 10).unwrap();
        session.write(b"hi");
        let second = session.snapshot().unwrap();
        assert_eq!(frame(&second, Some(&first))["full"], true);
    }

    #[test]
    fn a_frame_reports_the_scrollbar() {
        let mut session = soil_vt::Session::new(20, 3, 100).unwrap();
        session.write(b"a\r\nb\r\nc\r\nd\r\ne");
        session.scroll(-1);
        let frame = frame(&session.snapshot().unwrap(), None);
        let scroll = &frame["scroll"];
        assert!(scroll["total"].as_u64().unwrap() >= 5);
        assert!(scroll["offset"].as_u64().unwrap() < scroll["total"].as_u64().unwrap());
        assert!(frame["cursor"].is_null());
    }

    #[test]
    fn style_runs_merge_and_flags_are_reported() {
        let mut session = soil_vt::Session::new(20, 1, 10).unwrap();
        session.write(b"\x1b[1;31mred\x1b[0m plain");
        let frame = frame(&session.snapshot().unwrap(), None);
        let runs = frame["rows"][0]["runs"].as_array().unwrap();
        assert_eq!(runs[0][0], "red");
        assert_eq!(runs[0][3], 1);
        assert!(runs[1][0].as_str().unwrap().starts_with(" plain"));
        assert_eq!(runs[1][3], 0);
    }
}
