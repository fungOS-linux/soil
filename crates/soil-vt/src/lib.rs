//! Terminal emulation for Soil: bytes in, a grid out.
//!
//! Emulation is the part of a terminal that is actually hard — escape
//! sequences, wide characters, reflow on resize, scrollback, the alternate
//! screen. None of it is Soil's to invent, so this crate wraps
//! [`libghostty_vt`], the emulator extracted from Ghostty, and exposes the only
//! thing a renderer needs: a snapshot of the grid.
//!
//! ```text
//! guest bytes ──► Session::write ──► libghostty-vt ──► Snapshot ──► Canvas scene
//! ```
//!
//! Deliberately no rendering, no fonts, no colour policy. Soil owns the
//! process and its output; Canvas owns pixels. A `Snapshot` is plain data so it
//! can cross a WIT boundary unchanged.

pub mod host;

pub use host::Host;

use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::terminal::{Options, Terminal};

/// A colour as it arrives from the terminal, before any theme is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<libghostty_vt::style::RgbColor> for Rgb {
    fn from(value: libghostty_vt::style::RgbColor) -> Self {
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
}

impl Snapshot {
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        if x >= self.cols || y >= self.rows {
            return None;
        }
        self.cells.get(usize::from(y) * usize::from(self.cols) + usize::from(x))
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

impl From<libghostty_vt::error::Error> for Error {
    fn from(value: libghostty_vt::error::Error) -> Self {
        Self(value.to_string())
    }
}

/// A live terminal: the state a running program writes into.
pub struct Session {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
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
        Ok(Self {
            terminal: Terminal::new(Options {
                cols,
                rows,
                max_scrollback: scrollback,
            })?,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
        })
    }

    pub fn cols(&self) -> u16 {
        self.terminal.cols().unwrap_or(0)
    }

    pub fn rows(&self) -> u16 {
        self.terminal.rows().unwrap_or(0)
    }

    /// Feed a program's output. Partial sequences are held by the emulator, so
    /// a caller may hand over any chunk size, including single bytes.
    pub fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// Resize the grid. Content reflows; scrollback is preserved.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        if cols == 0 || rows == 0 {
            return Err(Error("a terminal needs at least one cell".into()));
        }
        self.terminal.resize(cols, rows, 1, 1)?;
        Ok(())
    }

    /// Read the grid as it should be drawn now.
    pub fn snapshot(&mut self) -> Result<Snapshot, Error> {
        let cols = self.cols();
        let rows = self.rows();
        let title = self.terminal.title().unwrap_or_default().to_owned();
        let cursor = self
            .terminal
            .is_cursor_visible()
            .unwrap_or(false)
            .then(|| {
                (
                    self.terminal.cursor_x().unwrap_or(0),
                    self.terminal.cursor_y().unwrap_or(0),
                )
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
        })
    }
}

fn read_cell(cell: &libghostty_vt::render::CellIteration<'_, '_>) -> Result<Cell, Error> {
    let mut text = String::new();
    cell.graphemes_utf8(&mut text)?;
    let style = cell.style()?;
    Ok(Cell {
        text,
        fg: cell.fg_color()?.map(Rgb::from),
        bg: cell.bg_color()?.map(Rgb::from),
        bold: style.bold,
        italic: style.italic,
        underline: style.underline != libghostty_vt::style::Underline::None,
        faint: style.faint,
        inverse: style.inverse,
        wide_tail: cell.raw_cell()?.wide()? != libghostty_vt::screen::CellWide::Narrow,
        selected: cell.is_selected()?,
    })
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
