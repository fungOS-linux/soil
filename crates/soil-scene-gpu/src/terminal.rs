//! Turn a `soil_vt::Snapshot` into a GPU scene.
//!
//! The terminal owns emulation; this module owns only cell placement. Background
//! runs become rects, text runs become text nodes, and the existing scene
//! renderer draws both with the glyph atlas. No second terminal ABI and no
//! per-cell canvas widget.

use std::sync::OnceLock;

use fontdue::Font;

use crate::{Frame, Renderer, Theme, load_mono_font};
use soil_scene::{Content, Ink, Node, Rect, Scene, Text, Transform};
use soil_vt::{Cell, Rgb, Snapshot};

static MONO: OnceLock<Option<Font>> = OnceLock::new();

fn terminal_font() -> Option<&'static Font> {
    MONO.get_or_init(load_mono_font).as_ref()
}

struct Grid {
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    font_size: f32,
}

fn grid(snapshot: &Snapshot, width: u32, height: u32) -> Grid {
    let cols = snapshot.cols.max(1) as f32;
    let rows = snapshot.rows.max(1) as f32;
    let margin = (width.min(height) as f32 * 0.018).clamp(10.0, 22.0);
    let available_w = (width as f32 - margin * 2.0).max(1.0);
    let available_h = (height as f32 - margin * 2.0).max(1.0);
    let cell_h = available_h / rows;
    let mut font_size = (cell_h * 0.78).max(7.0);
    let mut advance = terminal_font()
        .map(|font| font.metrics('M', font_size).advance_width)
        .unwrap_or(font_size * 0.6)
        .max(1.0);
    if advance * cols > available_w {
        let scale = available_w / (advance * cols);
        font_size = (font_size * scale).max(7.0);
        advance = terminal_font()
            .map(|font| font.metrics('M', font_size).advance_width)
            .unwrap_or(font_size * 0.6)
            .max(1.0);
    }
    let grid_w = advance * cols;
    Grid {
        x: (width as f32 - grid_w) * 0.5,
        y: margin,
        cell_w: advance,
        cell_h,
        font_size,
    }
}

/// Render a terminal snapshot into one frame.
pub fn render_terminal(
    snapshot: &Snapshot,
    width: u32,
    height: u32,
    theme: Theme,
) -> anyhow::Result<Frame> {
    let scene = terminal_scene(snapshot, width, height, theme);
    let mut renderer = Renderer::new()?;
    renderer.set_font(load_mono_font());
    renderer.render(&scene, width, height, theme)
}

/// Build the scene a terminal grid produces. Exposed for tests and previews.
pub fn terminal_scene(snapshot: &Snapshot, width: u32, height: u32, theme: Theme) -> Scene {
    let mut nodes = Vec::new();
    let grid = grid(snapshot, width, height);

    nodes.push(node_rect(
        "vt-ground",
        0.0,
        0.0,
        width as f32,
        height as f32,
        theme.background,
        width,
        height,
        0.0,
    ));

    for row in 0..snapshot.rows {
        let mut col = 0u16;
        while col < snapshot.cols {
            let Some(cell) = snapshot.cell(col, row) else {
                break;
            };
            let style = Style::from_cell(cell, theme);
            let start = col;
            let mut text = String::new();
            while col < snapshot.cols {
                let Some(next) = snapshot.cell(col, row) else {
                    break;
                };
                if Style::from_cell(next, theme) != style {
                    break;
                }
                if !next.wide_tail {
                    if next.text.is_empty() {
                        text.push(' ');
                    } else {
                        text.push_str(&next.text);
                    }
                }
                col += 1;
            }
            let x = grid.x + start as f32 * grid.cell_w;
            let y = grid.y + row as f32 * grid.cell_h;
            if style.bg != theme.background || style.selected {
                nodes.push(node_rect(
                    &format!("vt-bg-{row}-{start}"),
                    x,
                    y,
                    (col - start) as f32 * grid.cell_w,
                    grid.cell_h,
                    if style.selected {
                        theme.primary
                    } else {
                        style.bg
                    },
                    width,
                    height,
                    0.1,
                ));
            }
            if !text.trim().is_empty() {
                nodes.push(node_text(
                    &format!("vt-text-{row}-{start}"),
                    &text,
                    x,
                    y,
                    grid.font_size,
                    if style.selected {
                        theme.background
                    } else {
                        style.fg
                    },
                    width,
                    height,
                    theme,
                    0.2,
                ));
            }
        }
    }

    if let Some((cursor_x, cursor_y)) = snapshot.cursor {
        nodes.push(node_rect(
            "vt-cursor",
            grid.x + cursor_x as f32 * grid.cell_w,
            grid.y + cursor_y as f32 * grid.cell_h,
            grid.cell_w,
            grid.cell_h,
            theme.primary,
            width,
            height,
            0.3,
        ));
    }

    Scene::new(nodes)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: [u8; 3],
    bg: [u8; 3],
    selected: bool,
}

impl Style {
    fn from_cell(cell: &Cell, theme: Theme) -> Self {
        let mut fg = cell.fg.map(rgb).unwrap_or(theme.text);
        let mut bg = cell.bg.map(rgb).unwrap_or(theme.background);
        if cell.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.selected {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.faint {
            fg = mix(fg, theme.background, 0.45);
        }
        Self {
            fg,
            bg,
            selected: cell.selected,
        }
    }
}

fn rgb(value: Rgb) -> [u8; 3] {
    [value.r, value.g, value.b]
}

fn mix(a: [u8; 3], b: [u8; 3], amount: f32) -> [u8; 3] {
    [
        (a[0] as f32 * (1.0 - amount) + b[0] as f32 * amount) as u8,
        (a[1] as f32 * (1.0 - amount) + b[1] as f32 * amount) as u8,
        (a[2] as f32 * (1.0 - amount) + b[2] as f32 * amount) as u8,
    ]
}

#[allow(clippy::too_many_arguments)]
fn node_rect(
    id: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [u8; 3],
    width: u32,
    height: u32,
    z: f32,
) -> Node {
    Node {
        id: id.into(),
        parent: None,
        transition_ms: None,
        transform: Transform {
            translation: [
                -4.0 + 8.0 * x / width as f32,
                4.0 - 8.0 * y / height as f32,
                z,
            ],
            rotation: [0.0; 3],
            scale: [8.0 * w / width as f32, 8.0 * h / height as f32, 1.0],
        },
        content: Content::Rect(Rect {
            ink: None,
            rgb: Some(color),
            opacity: 1.0,
            radius: 0.0,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn node_text(
    id: &str,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    color: [u8; 3],
    width: u32,
    height: u32,
    theme: Theme,
    z: f32,
) -> Node {
    let scale = size / (theme.body * 1.25).max(1.0);
    Node {
        id: id.into(),
        parent: None,
        transition_ms: None,
        transform: Transform {
            translation: [
                -4.0 + 8.0 * x / width as f32,
                4.0 - 8.0 * y / height as f32,
                z,
            ],
            rotation: [0.0; 3],
            scale: [scale, scale, 1.0],
        },
        content: Content::Text(Text {
            text: text.into(),
            ink: Some(Ink::Text),
            rgb: Some(color),
            opacity: 1.0,
        }),
    }
}
