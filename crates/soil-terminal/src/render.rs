//! Painting a terminal grid into pixels.
//!
//! Separated from the window on purpose: the same painter draws the live window
//! and an offscreen dump, so what a screenshot shows is what the window shows.
//! A renderer that only exists inside an event loop cannot be tested, and a
//! terminal whose colours you cannot verify is a terminal whose colours are
//! probably wrong.

use fontdue::Font;
use soil_vt::{Cell, Snapshot};

/// The faces a cell can ask for. Missing cuts fall back rather than disappear,
/// because a missing italic face should not make text vanish.
pub struct Faces {
    pub regular: Font,
    pub bold: Option<Font>,
    pub italic: Option<Font>,
    pub bold_italic: Option<Font>,
}

impl Faces {
    fn pick(&self, cell: &Cell) -> &Font {
        match (cell.bold, cell.italic) {
            (true, true) => self
                .bold_italic
                .as_ref()
                .or(self.bold.as_ref())
                .or(self.italic.as_ref())
                .unwrap_or(&self.regular),
            (true, false) => self.bold.as_ref().unwrap_or(&self.regular),
            (false, true) => self.italic.as_ref().unwrap_or(&self.regular),
            (false, false) => &self.regular,
        }
    }
}

pub struct Metrics {
    pub cell_w: u32,
    pub cell_h: u32,
    pub px_size: f32,
    pub ascent: f32,
}

impl Metrics {
    pub fn new(font: &Font, point_size: f32, scale: f32) -> Self {
        // Everything is measured at the *device* size. Measuring the advance at
        // the point size and then multiplying by the scale applies the display
        // scale twice, which is how a 15pt cell became 30pt tall and left a
        // screen of text with gaps you could park a car in.
        let px_size = point_size * scale;
        let advance = font.metrics('M', px_size).advance_width.max(1.0);
        let height = font
            .horizontal_line_metrics(px_size)
            .map(|line| line.new_line_size)
            .unwrap_or(px_size * 1.6);
        Self {
            // Whole device pixels: a fractional advance makes every other column
            // drift by a subpixel, which in a column of text is glaring.
            cell_w: advance.round().max(1.0) as u32,
            cell_h: height.round().max(1.0) as u32,
            px_size,
            ascent: font
                .horizontal_line_metrics(px_size)
                .map(|line| line.ascent)
                .unwrap_or(px_size * 0.8),
        }
    }
}

const DEFAULT_FG: u32 = 0x00d0_d0d0;

/// Selection is an inversion, not a wash: an overlay lightens the glyphs it is
/// meant to make easier to read.
const SELECTION: u32 = 0x00_40_5a_80;

pub fn paint(
    snapshot: &Snapshot,
    faces: &Faces,
    metrics: &Metrics,
    width: u32,
    height: u32,
    buffer: &mut [u32],
) {
    buffer.fill(0x0000_0000);
    for (index, cell) in snapshot.cells.iter().enumerate() {
        let x = (index % usize::from(snapshot.cols)) as u32;
        let y = (index / usize::from(snapshot.cols)) as u32;
        let (px, py) = (x * metrics.cell_w, y * metrics.cell_h);
        if px + metrics.cell_w > width || py + metrics.cell_h > height {
            continue;
        }
        let (mut fg, mut bg) = resolve(cell);
        if cell.selected {
            std::mem::swap(&mut fg, &mut bg);
            if bg == 0 {
                bg = SELECTION;
            }
        }
        fill_cell(buffer, width, px, py, metrics.cell_w, metrics.cell_h, bg);
        if cell.text.is_empty() {
            continue;
        }
        let face = faces.pick(cell);
        let (fr, fg_g, fb) = unpack(fg);
        let mut advance = 0.0f32;
        for ch in cell.text.chars() {
            if ch == '\u{200b}' {
                continue;
            }
            let (glyph, coverage) = face.rasterize(ch, metrics.px_size);
            let origin_x = px as f32 + advance + glyph.xmin as f32;
            let origin_y = py as f32 + metrics.ascent - glyph.ymin as f32 - glyph.height as f32;
            for (row, line) in coverage.chunks(glyph.width.max(1)).enumerate() {
                for (col, alpha) in line.iter().enumerate() {
                    if *alpha == 0 {
                        continue;
                    }
                    let tx = origin_x as i32 + col as i32;
                    let ty = origin_y as i32 + row as i32;
                    if tx < 0 || ty < 0 || tx >= width as i32 || ty >= height as i32 {
                        continue;
                    }
                    blend(
                        &mut buffer[ty as usize * width as usize + tx as usize],
                        (fr, fg_g, fb),
                        *alpha,
                    );
                }
            }
            advance += glyph.advance_width;
        }
        if cell.underline {
            let top = py + (metrics.ascent + 1.0) as u32;
            let thickness = (metrics.px_size / 12.0).ceil().max(1.0) as u32;
            for dy in 0..thickness {
                let row = top + dy;
                if row >= height {
                    break;
                }
                for dx in 0..metrics.cell_w {
                    let col = px + dx;
                    if col >= width {
                        break;
                    }
                    buffer[row as usize * width as usize + col as usize] = fg;
                }
            }
        }
    }
}

/// Draw the cursor as a filled block with the character punched back out.
pub fn paint_cursor(
    snapshot: &Snapshot,
    faces: &Faces,
    metrics: &Metrics,
    width: u32,
    height: u32,
    buffer: &mut [u32],
    on: bool,
) {
    if !on {
        return;
    }
    let Some((cx, cy)) = snapshot.cursor else {
        return;
    };
    let (px, py) = (
        u32::from(cx) * metrics.cell_w,
        u32::from(cy) * metrics.cell_h,
    );
    if px + metrics.cell_w > width || py + metrics.cell_h > height {
        return;
    }
    let caret = 0x00d0_d0d0;
    fill_cell(buffer, width, px, py, metrics.cell_w, metrics.cell_h, caret);
    let Some(cell) = snapshot.cell(cx, cy) else {
        return;
    };
    let Some(ch) = cell.text.chars().next() else {
        return;
    };
    let face = faces.pick(cell);
    let (glyph, coverage) = face.rasterize(ch, metrics.px_size);
    let origin_x = px as f32 + glyph.xmin as f32;
    let origin_y = py as f32 + metrics.ascent - glyph.ymin as f32 - glyph.height as f32;
    for (row, line) in coverage.chunks(glyph.width.max(1)).enumerate() {
        for (col, alpha) in line.iter().enumerate() {
            if *alpha == 0 {
                continue;
            }
            let tx = origin_x as i32 + col as i32;
            let ty = origin_y as i32 + row as i32;
            if tx < 0 || ty < 0 || tx >= width as i32 || ty >= height as i32 {
                continue;
            }
            blend(
                &mut buffer[ty as usize * width as usize + tx as usize],
                (0, 0, 0),
                *alpha,
            );
        }
    }
}

fn fill_cell(buffer: &mut [u32], width: u32, px: u32, py: u32, w: u32, h: u32, color: u32) {
    for dy in 0..h {
        let row = (py + dy) as usize * width as usize;
        for dx in 0..w {
            let index = row + (px + dx) as usize;
            if index < buffer.len() {
                buffer[index] = color;
            }
        }
    }
}

/// A cell's colours. The terminal's explicit colour wins; otherwise the default
/// pair, which keeps a themed fallback while honouring a program that asked for
/// something specific.
fn resolve(cell: &Cell) -> (u32, u32) {
    let (mut fg, mut bg) = match (cell.fg, cell.bg) {
        (Some(fg), Some(bg)) => (rgb(fg), rgb(bg)),
        (Some(fg), None) => (rgb(fg), 0),
        (None, Some(bg)) => (DEFAULT_FG, rgb(bg)),
        (None, None) => (DEFAULT_FG, 0),
    };
    if cell.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.faint {
        fg = dim(fg);
    }
    (fg, bg)
}

fn rgb(color: soil_vt::Rgb) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

fn dim(color: u32) -> u32 {
    let (r, g, b) = unpack(color);
    (u32::from(r / 2) << 16) | (u32::from(g / 2) << 8) | u32::from(b / 2)
}

fn unpack(color: u32) -> (u8, u8, u8) {
    (
        (color >> 16 & 0xff) as u8,
        (color >> 8 & 0xff) as u8,
        (color & 0xff) as u8,
    )
}

fn blend(pixel: &mut u32, fg: (u8, u8, u8), alpha: u8) {
    let (br, bg, bb) = unpack(*pixel);
    let a = u32::from(alpha);
    let inv = 255 - a;
    let mix = |back: u8, front: u8| -> u32 { (u32::from(back) * inv + u32::from(front) * a) / 255 };
    *pixel = (mix(br, fg.0) << 16) | (mix(bg, fg.1) << 8) | mix(bb, fg.2);
}
