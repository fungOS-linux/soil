//! Terminal emulation for Soil: bytes in, a grid out.
//!
//! Emulation is the part of a terminal that is actually hard — escape
//! sequences, wide characters, reflow on resize, scrollback, the alternate
//! screen. None of it is Soil's to invent, so this crate wraps
//! [`ghostty_vt`], the emulator extracted from Ghostty, and exposes the only
//! thing a renderer needs: a snapshot of the grid.
//!
//! ```text
//! guest bytes ──► Session::write ──► libghostty-vt ──► Snapshot ──► Canvas scene
//! ```
//!
//! Deliberately no rendering, no fonts, no colour policy. Soil owns the
//! process and its output; Canvas owns pixels. A `Snapshot` is plain data so it
//! can cross a WIT boundary unchanged.
//!
//! # Building
//!
//! `ghostty-vt-sys` compiles libghostty-vt with Zig, and Ghostty requires an
//! exact Zig minor version. The toolchain is pinned in `.cargo/config.toml`
//! (`ZIG`) and fetched by `scripts/bootstrap-toolchain.sh`, so `cargo build`
//! does not depend on whatever `zig` happens to be on `PATH`.
//!
//! ```sh
//! scripts/bootstrap-toolchain.sh
//! cargo test -p soil-vt
//! ```
//!
//! If the build fails inside Ghostty's `libsystem_override.sh` with
//! `ar: temporary file: No such file or directory`, the process `TMPDIR` points
//! at a directory that no longer exists. Re-run with `TMPDIR=/tmp`.

pub mod host;

pub use host::Host;

use ghostty_vt::render::{CellIterator, RenderState, RowIterator};
use ghostty_vt::terminal::{Event, Options, ScrollViewport, Terminal};

/// A colour as it arrives from the terminal, before any theme is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<ghostty_vt::style::RgbColor> for Rgb {
    fn from(value: ghostty_vt::style::RgbColor) -> Self {
        Self {
            r: value.r,
            g: value.g,
            b: value.b,
        }
    }
}

/// One cell of the grid.
///
/// `text` is a full grapheme cluster, not a `char`: an emoji with a skin tone
/// modifier occupies one cell and must stay one cell all the way to the
/// renderer. Empty cells carry no text rather than a space, so a renderer can
/// tell "blank" from "deliberately a space".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    /// Set only when the terminal asked for a specific colour; `None` means
    /// the theme decides.
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub faint: bool,
    pub inverse: bool,
    /// The cell to the left of this one holds a wide cluster, so a renderer
    /// must not advance twice for it.
    pub wide_tail: bool,
    /// Part of the terminal's selection. A renderer draws selection as a colour
    /// inversion rather than as an overlay, because an overlay would hide the
    /// glyphs it is supposed to make easier to read.
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    /// Row-major, `cols * rows` cells.
    pub cells: Vec<Cell>,
    pub cursor: Option<(u16, u16)>,
    pub title: String,
    /// Bells rung so far, cumulative. A renderer compares this with the value it
    /// last acted on; a counter survives a dropped frame where a flag would not.
    pub bells: u64,
    /// Where the viewport sits in the scrollback, when the emulator reports it.
    /// A renderer uses this for a scrollbar and to tell history from the active
    /// screen; `None` means the emulator did not answer.
    pub scroll: Option<Scroll>,
}

/// The viewport's position in the scrollable area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scroll {
    /// First visible row, counted from the top of the scrollback.
    pub offset: u64,
    /// Total rows in the scrollable area.
    pub total: u64,
    /// Rows visible at once.
    pub len: u64,
}

impl Snapshot {
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        if x >= self.cols || y >= self.rows {
            return None;
        }
        self.cells
            .get(usize::from(y) * usize::from(self.cols) + usize::from(x))
    }

    /// The visible text of a row, trimmed. Useful for tests and for a status
    /// line; never for rendering, which needs the cells.
    pub fn row_text(&self, y: u16) -> String {
        let mut out = String::new();
        let Some(base) = self.cells.get(usize::from(y) * usize::from(self.cols)) else {
            return out;
        };
        let _ = base;
        for x in 0..self.cols {
            match self.cell(x, y) {
                Some(cell) if !cell.text.is_empty() => out.push_str(&cell.text),
                Some(_) => out.push(' '),
                None => break,
            }
        }
        out.trim_end().to_owned()
    }
}

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<ghostty_vt::error::Error> for Error {
    fn from(value: ghostty_vt::error::Error) -> Self {
        Self(value.to_string())
    }
}

/// A live terminal: the state a running program writes into.
pub struct Session {
    terminal: Terminal,
    /// Bells rung so far. The emulator reports each bell as an event while it
    /// parses; a counter survives a dropped frame where a flag would not.
    bells: u64,
    /// Bytes the emulator wants written back to the pty (device status reports,
    /// and the kitty keyboard negotiation that a full-screen program expects).
    responses: Vec<u8>,
    render: RenderState,
    rows: RowIterator,
    cells: CellIterator,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("cols", &self.cols())
            .field("rows", &self.rows())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// `scrollback` is a line count, not bytes: an emulator's memory is bounded
    /// by what a person can scroll back to.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Result<Self, Error> {
        if cols == 0 || rows == 0 {
            return Err(Error("a terminal needs at least one cell".into()));
        }
        let mut session = Self {
            terminal: Terminal::new(Options {
                cols,
                rows,
                max_scrollback: scrollback_bytes(scrollback, cols),
            })?,
            bells: 0,
            responses: Vec::new(),
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
        };
        // Creating a terminal already queues its device-attribute answer.
        session.pump_events();
        Ok(session)
    }

    pub fn cols(&self) -> u16 {
        self.terminal.cols().unwrap_or(0)
    }

    pub fn rows(&self) -> u16 {
        self.terminal.rows().unwrap_or(0)
    }

    /// Bells rung since the session started.
    pub fn bells(&self) -> u64 {
        self.bells
    }

    /// Move the viewport through history. Negative is up, towards older rows.
    pub fn scroll(&mut self, delta: isize) {
        self.terminal
            .scroll_viewport(ScrollViewport::Delta(delta));
    }

    /// Return the viewport to the active screen.
    pub fn scroll_to_bottom(&mut self) {
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
    }

    /// Take the bytes the emulator wants sent back to the program, if any.
    pub fn drain_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    /// Feed a program's output. Partial sequences are held by the emulator, so
    /// a caller may hand over any chunk size, including single bytes.
    pub fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
        self.pump_events();
    }

    /// Resize the grid. Content reflows; scrollback is preserved.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        if cols == 0 || rows == 0 {
            return Err(Error("a terminal needs at least one cell".into()));
        }
        self.terminal.resize(cols, rows, 1, 1)?;
        // A resize can emit a size report for a program that asked for one.
        self.pump_events();
        Ok(())
    }

    /// Collect what the emulator queued while parsing: bells, and bytes a
    /// program is waiting on. Events are drained after every write so the
    /// outbox never grows with output nobody asked for.
    fn pump_events(&mut self) {
        for event in self.terminal.events() {
            match event {
                Event::Bell => self.bells = self.bells.saturating_add(1),
                Event::PtyWrite(bytes) => self.responses.extend_from_slice(&bytes),
                _ => {}
            }
        }
    }

    /// Read the grid as it should be drawn now.
    pub fn snapshot(&mut self) -> Result<Snapshot, Error> {
        let cols = self.cols();
        let rows = self.rows();
        let title = self.terminal.title().unwrap_or_default().to_owned();
        // A cursor in history would point at a row that is not the program's
        // cursor, so it is only meaningful while the viewport is active.
        let viewport_active = self.terminal.is_viewport_active().unwrap_or(true);
        let cursor = (viewport_active && self.terminal.is_cursor_visible().unwrap_or(false)).then(
            || {
                (
                    self.terminal.cursor_x().unwrap_or(0),
                    self.terminal.cursor_y().unwrap_or(0),
                )
            },
        );
        let scroll = self.terminal.scrollbar().ok().map(|bar| Scroll {
            offset: bar.offset,
            total: bar.total,
            len: bar.len,
        });

        let mut cells = vec![Cell::default(); usize::from(cols) * usize::from(rows)];
        {
            let snapshot = self.render.update(&self.terminal)?;
            let mut row_iteration = self.rows.update(&snapshot)?;
            let mut y = 0u16;
            while let Some(row) = row_iteration.next() {
                let mut cell_iteration = self.cells.update(row)?;
                let mut x = 0u16;
                while let Some(cell) = cell_iteration.next() {
                    if x < cols && y < rows {
                        cells[usize::from(y) * usize::from(cols) + usize::from(x)] =
                            read_cell(cell)?;
                    }
                    x = x.saturating_add(1);
                }
                y = y.saturating_add(1);
            }
        }

        Ok(Snapshot {
            cols,
            rows,
            cells,
            cursor,
            title,
            bells: self.bells,
            scroll,
        })
    }
}

fn read_cell(cell: &ghostty_vt::render::CellIteration<'_>) -> Result<Cell, Error> {
    let mut text = String::new();
    cell.graphemes_utf8(&mut text)?;
    let style = cell.style()?;
    Ok(Cell {
        text,
        fg: cell.fg_color()?.map(Rgb::from),
        bg: cell.bg_color()?.map(Rgb::from),
        bold: style.bold,
        italic: style.italic,
        underline: style.underline != ghostty_vt::style::Underline::None,
        faint: style.faint,
        inverse: style.inverse,
        // Only the *tail* of a wide cluster is a tail. The leading cell owns
        // the glyph and advances the renderer by two; marking it here too
        // would make a renderer skip the glyph it is supposed to draw.
        wide_tail: cell.raw_cell()?.wide()? == ghostty_vt::screen::CellWide::SpacerTail,
        selected: cell.is_selected()?,
    })
}

/// Ghostty bounds scrollback in bytes, pruned at page granularity; callers of
/// this crate think in lines. Sixteen bytes per cell approximates one styled
/// grapheme, which keeps the bound proportional to the requested line count
/// without claiming to be exact.
fn scrollback_bytes(lines: usize, cols: u16) -> usize {
    lines
        .saturating_mul(usize::from(cols))
        .saturating_mul(16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(20, 4, 100).expect("terminal")
    }

    #[test]
    fn plain_text_lands_in_cells() {
        let mut session = session();
        session.write(b"hello");
        let snapshot = session.snapshot().unwrap();
        assert_eq!(snapshot.row_text(0), "hello");
        assert_eq!(snapshot.cell(0, 0).unwrap().text, "h");
        assert_eq!(snapshot.cursor, Some((5, 0)));
    }

    #[test]
    fn a_bell_is_counted_and_survives_into_the_snapshot() {
        let mut session = Session::new(10, 2, 0).unwrap();
        assert_eq!(session.bells(), 0);
        session.write(b"\x07");
        assert_eq!(session.bells(), 1);
        session.write(b"ab\x07\x07");
        assert_eq!(session.snapshot().unwrap().bells, 3);
    }

    #[test]
    fn the_emulator_answers_a_device_status_report() {
        let mut session = Session::new(10, 2, 0).unwrap();
        session.write(b"\x1b[6n");
        let response = session.drain_responses();
        assert!(!response.is_empty(), "a cursor position report is expected");
        assert!(String::from_utf8_lossy(&response).contains('R'));
        // Taking the bytes clears them: a caller that writes them to the pty
        // must not send them twice.
        assert!(session.drain_responses().is_empty());
    }

    #[test]
    fn newline_and_carriage_return_move_the_cursor() {
        let mut session = session();
        session.write(b"one\r\ntwo");
        let snapshot = session.snapshot().unwrap();
        assert_eq!(snapshot.row_text(0), "one");
        assert_eq!(snapshot.row_text(1), "two");
    }

    #[test]
    fn sgr_colours_stay_with_their_cells() {
        let mut session = session();
        // Red foreground, then default.
        session.write(b"\x1b[31mred\x1b[0m plain");
        let snapshot = session.snapshot().unwrap();
        let red = snapshot.cell(0, 0).unwrap();
        assert!(red.fg.is_some(), "expected an explicit foreground");
        let plain = snapshot.cell(4, 0).unwrap();
        assert!(plain.fg.is_none(), "reset should clear the explicit colour");
    }

    #[test]
    fn bold_and_underline_are_attributes_not_text() {
        let mut session = session();
        session.write(b"\x1b[1;4mstyled\x1b[0m");
        let snapshot = session.snapshot().unwrap();
        let cell = snapshot.cell(0, 0).unwrap();
        assert!(cell.bold && cell.underline);
        assert_eq!(cell.text, "s");
    }

    #[test]
    fn a_split_escape_sequence_is_buffered_across_writes() {
        let mut session = session();
        session.write(b"a\x1b[3");
        session.write(b"1mb");
        let snapshot = session.snapshot().unwrap();
        assert_eq!(snapshot.row_text(0), "ab");
        assert!(snapshot.cell(1, 0).unwrap().fg.is_some());
    }

    #[test]
    fn resizing_reflows_and_keeps_content() {
        let mut session = session();
        session.write(b"abcdef");
        session.resize(3, 4).unwrap();
        let snapshot = session.snapshot().unwrap();
        assert_eq!(snapshot.cols, 3);
        assert_eq!(snapshot.row_text(0), "abc");
        assert_eq!(snapshot.row_text(1), "def");
    }

    #[test]
    fn italic_is_an_attribute_and_selection_defaults_to_empty() {
        let mut session = session();
        session.write(b"\x1b[3mslanted\x1b[0m plain");
        let snapshot = session.snapshot().unwrap();
        assert!(snapshot.cell(0, 0).unwrap().italic);
        assert!(!snapshot.cell(8, 0).unwrap().italic);
        // Nothing is selected until a gesture says so; a renderer must be able
        // to rely on the flag being meaningful rather than always true.
        assert!(!snapshot.cell(0, 0).unwrap().selected);
    }

    #[test]
    fn only_the_trailing_cell_of_a_wide_cluster_is_a_tail() {
        let mut session = Session::new(4, 1, 10).expect("terminal");
        session.write("界".as_bytes());
        let snapshot = session.snapshot().unwrap();
        let head = snapshot.cell(0, 0).unwrap();
        assert_eq!(head.text, "界");
        assert!(
            !head.wide_tail,
            "the leading cell owns the glyph and must be drawn"
        );
        assert!(
            snapshot.cell(1, 0).unwrap().wide_tail,
            "the spacer to the right is the tail"
        );
    }

    #[test]
    fn a_ratatui_frame_lands_in_the_grid() {
        // Produced by `spores/tui` (`App::preview(60, 16, 0)`) — the TUI Spore
        // renders ANSI, and this is the host half of the contract.
        let frame = include_bytes!("../tests/fixtures/ratatui-frame.ansi");
        let mut session = Session::new(60, 16, 1000).expect("terminal");
        session.write(frame);
        let snapshot = session.snapshot().unwrap();
        assert_eq!(snapshot.cols, 60);
        assert_eq!(snapshot.rows, 16);
        assert!(snapshot.row_text(0).starts_with("┌ soil tui ─"), "{}", snapshot.row_text(0));
        assert!(snapshot.row_text(1).contains("Overview"), "{}", snapshot.row_text(1));
        assert!(snapshot.row_text(4).contains("> Ingest"), "{}", snapshot.row_text(4));
        assert!(snapshot.row_text(14).contains("tab switch"), "{}", snapshot.row_text(14));
        // The frame hides the cursor at the end, so the snapshot carries none.
        assert_eq!(snapshot.cursor, None);
    }

    #[test]
    fn scrolling_up_shows_earlier_rows_and_hides_the_cursor() {
        let mut session = Session::new(20, 3, 100).expect("terminal");
        session.write(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");
        let active = session.snapshot().unwrap();
        assert_eq!(active.row_text(0), "l3");
        assert_eq!(active.row_text(2), "l5");
        assert!(active.cursor.is_some(), "the active screen shows a cursor");
        let active_scroll = active.scroll.expect("a scrollbar state");
        assert_eq!(active_scroll.len, 3);

        session.scroll(-2);
        let history = session.snapshot().unwrap();
        assert_eq!(history.row_text(0), "l1");
        assert_eq!(history.row_text(2), "l3");
        assert_eq!(history.cursor, None, "no cursor while reading history");
        assert!(
            history.scroll.expect("scrollbar").offset < active_scroll.offset,
            "the viewport moved up"
        );

        session.scroll_to_bottom();
        let back = session.snapshot().unwrap();
        assert_eq!(back.row_text(0), "l3");
        assert!(back.cursor.is_some());
    }

    #[test]
    fn scrolling_past_the_ends_is_clamped() {
        let mut session = Session::new(20, 3, 100).expect("terminal");
        session.write(b"a\r\nb\r\nc\r\nd");
        session.scroll(-10_000);
        let top = session.snapshot().unwrap();
        assert_eq!(top.row_text(0), "a");
        session.scroll(10_000);
        let bottom = session.snapshot().unwrap();
        assert_eq!(bottom.row_text(0), "b");
    }

    #[test]
    fn zero_sized_terminal_is_rejected() {
        assert!(Session::new(0, 10, 100).is_err());
        assert!(Session::new(10, 0, 100).is_err());
    }
}

#[cfg(test)]
mod colour_tests {
    use super::*;

    fn cell_after(sequence: &[u8]) -> Cell {
        let mut session = Session::new(20, 2, 10).expect("terminal");
        session.write(sequence);
        session.snapshot().unwrap().cell(0, 0).cloned().unwrap()
    }

    #[test]
    fn indexed_256_colours_resolve_to_rgb() {
        // 196 is the top of the 6x6x6 cube: pure red. A terminal that only
        // supported the basic sixteen would report None here.
        let cell = cell_after(b"\x1b[38;5;196mX");
        let fg = cell.fg.expect("256-colour foreground resolves to rgb");
        assert!(fg.r > 200 && fg.g < 60 && fg.b < 60, "{fg:?}");
    }

    #[test]
    fn truecolor_is_not_quantised() {
        let cell = cell_after(b"\x1b[38;2;1;2;3mX");
        let fg = cell.fg.expect("truecolor foreground");
        assert_eq!((fg.r, fg.g, fg.b), (1, 2, 3));
    }

    #[test]
    fn background_and_foreground_are_independent() {
        let cell = cell_after(b"\x1b[48;5;21m\x1b[38;5;226mX");
        let fg = cell.fg.expect("foreground");
        let bg = cell.bg.expect("background");
        assert!(fg.r > 200 && fg.g > 200, "yellow fg: {fg:?}");
        assert!(bg.b > 150, "blue bg: {bg:?}");
    }
}
