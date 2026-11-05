//! A terminal in a native window.
//!
//! The same `soil-vt` grid Canvas draws, but presented by Soil itself: this is
//! the "one Spore, a different host" claim made concrete. The compositor is not
//! involved, so this binary is also the honest test of whether terminal
//! emulation and the process plumbing stand on their own.

mod encode;

use std::io::{Read, Write};
use std::sync::mpsc::{TryRecvError, channel, Receiver};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use fontdue::{Font, FontSettings};
use soil_vt::Session;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

/// Point size the grid is laid out at. The cell is derived from the font's own
/// advance, so the column count is always exactly what the font can fit.
const FONT_SIZE: f32 = 15.0;

/// The face, in preference order. Someone with a Nerd Font installed asked for
/// it; anyone else gets the system's own monospaced face rather than a bitmap.
const BOLD_CANDIDATES: &[&str] = &[
    "Library/Fonts/Fira Code Bold Nerd Font Complete Mono.otf",
    "Library/Fonts/Fira Code Semibold Nerd Font Complete Mono.otf",
    "Library/Fonts/FiraCodeNerdFontMono-Bold.ttf",
    "Library/Fonts/Fira Code Bold Nerd Font Complete.otf",
    "usr/share/fonts/truetype/firacode/FiraCode-Bold.ttf",
];

const FONT_CANDIDATES: &[&str] = &[
    "Library/Fonts/Fira Code Regular Nerd Font Complete Mono.otf",
    "Library/Fonts/FiraCodeNerdFontMono-Regular.ttf",
    "Library/Fonts/Fira Code Regular Nerd Font Complete.otf",
    "Library/Fonts/FiraCode-Regular.ttf",
    ".local/share/fonts/FiraCodeNerdFontMono-Regular.ttf",
    "usr/share/fonts/truetype/firacode/FiraCode-Regular.ttf",
];

/// Resolve a font file from the candidate list, or `SOIL_TERMINAL_FONT`.
fn read_font(candidates: &[&str]) -> Option<Vec<u8>> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    for candidate in candidates {
        let path = match (&home, candidate.starts_with("Library") || candidate.starts_with('.')) {
            (Some(home), true) => home.join(candidate),
            _ => std::path::PathBuf::from("/").join(candidate),
        };
        if let Ok(read) = std::fs::read(&path) {
            return Some(read);
        }
    }
    None
}

fn load_font() -> (Font, Option<Font>, f32, f32) {
    let bytes = std::env::var_os("SOIL_TERMINAL_FONT")
        .and_then(|path| std::fs::read(path).ok())
        .or_else(|| read_font(FONT_CANDIDATES));
    // Bold is a different face, not a heavier rasterisation: a terminal that
    // fakes bold by smearing the regular face is how text becomes mush.
    let bold = read_font(BOLD_CANDIDATES)
        .and_then(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok());
    let font = bytes
        .and_then(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok())
        .expect("a terminal needs a font: set SOIL_TERMINAL_FONT to a .ttf or .otf");
    // Cell geometry comes from the font: advance is the column width, and the
    // line box is ascent + descent, so a row is as tall as the face says.
    let probe = font.metrics('M', FONT_SIZE);
    let advance = probe.advance_width.max(1.0).round();
    let height = (probe.height as f32).max(FONT_SIZE).ceil();
    (font, bold, advance, height)
}

/// What the window is for.
///
/// `--text` exists so a host can show a *diagnostic* in the same emulator a
/// program writes to: a failed module reload, a boot log, a crash traceback.
/// Terminal output is the native format for "something went wrong at line 484",
/// and it means an error can be read the same way in a window as in a log.
#[derive(Debug, Clone, Default)]
struct Options {
    title: Option<String>,
    text: Option<String>,
    command: Vec<String>,
}

fn parse() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--title" => options.title = args.next(),
            "--text" => options.text = args.next(),
            "--command" => {
                options.command = args.by_ref().collect();
                break;
            }
            other => {
                options.command.push(other.to_owned());
            }
        }
    }
    options
}

/// A program on a pseudo-terminal.
///
/// A pipe would be simpler and wrong: nothing on a pipe has a window size, so a
/// full-screen program never learns it was resized and the display freezes at
/// whatever it drew first. A pty is what makes shrinking a window *reflow* rather
/// than crop.
struct Terminal {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Option<Box<dyn Write + Send>>,
    output: Option<Receiver<Vec<u8>>>,
}

impl Terminal {
    fn spawn(command: &[String], cols: u16, rows: u16) -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open a pty");
        let mut builder = CommandBuilder::new(&command[0]);
        builder.args(&command[1..]);
        builder.env("TERM", "xterm-256color");
        // A terminal without these is a terminal where `ls` is one colour and
        // every TUI warns about a dumb terminal.
        builder.env("COLORTERM", "truecolor");
        builder.env("TERM_PROGRAM", "soil-terminal");
        let child = pair.slave.spawn_command(builder).expect("spawn the guest");
        // The slave belongs to the child now; holding it open keeps the pty from
        // signalling end-of-file when the child exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer = pair.master.take_writer().expect("pty writer");
        let (sender, output) = channel();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sender.send(buffer[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });

        Self {
            master: pair.master,
            child,
            writer: Some(writer),
            output: Some(output),
        }
    }

    /// Tell the kernel — and therefore the program — that its window changed.
    /// This is the whole point of the pty: the ioctl raises `SIGWINCH`, and the
    /// program decides how to reflow.
    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn write(&mut self, bytes: &[u8]) {
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
    }
}


struct App {
    options: Options,
    /// Backing-store scale, so a 15pt cell is 15pt on a Retina display too.
    scale: f32,
    window: Option<std::sync::Arc<Window>>,
    context: Option<softbuffer::Context<winit::event_loop::OwnedDisplayHandle>>,
    surface: Option<
        softbuffer::Surface<winit::event_loop::OwnedDisplayHandle, std::sync::Arc<Window>>,
    >,
    /// The emulator: what the bytes mean.
    terminal: Option<Session>,
    /// The process: the pty it runs on, and the bytes it produces.
    process: Option<Terminal>,
    modifiers: ModifiersState,
    dirty: bool,
    font: Option<Font>,
    font_bold: Option<Font>,
    cell_w: f32,
    cell_h: f32,
}

impl Default for App {
    fn default() -> Self {
        Self {
            options: Options::default(),
            scale: 1.0,
            window: None,
            context: None,
            surface: None,
            terminal: None,
            process: None,
            modifiers: ModifiersState::empty(),
            dirty: true,
            font: None,
            font_bold: None,
            cell_w: 8.0,
            cell_h: 16.0,
        }
    }
}

impl App {
    fn grid_size(&self, size: PhysicalSize<u32>) -> (u16, u16) {
        let scale = self.scale;
        let cell_w = (self.cell_w * scale).max(1.0);
        let cell_h = (self.cell_h * scale).max(1.0);
        (
            ((size.width as f32 / cell_w).floor().max(1.0)) as u16,
            ((size.height as f32 / cell_h).floor().max(1.0)) as u16,
        )
    }

    fn send(&mut self, bytes: &[u8]) {
        if let Some(process) = self.process.as_mut() {
            process.write(bytes);
        }
    }

    fn kill(&mut self) {
        if let Some(process) = self.process.as_mut() {
            process.kill();
        }
    }

    fn resize_terminal(&mut self, size: PhysicalSize<u32>) {
        let (cols, rows) = self.grid_size(size);
        let unchanged = self
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.cols() == cols && terminal.rows() == rows);
        if unchanged {
            return;
        }
        if let Some(terminal) = self.terminal.as_mut() {
            let _ = terminal.resize(cols, rows);
        }
        // Both sides, always: the emulator reflows its own grid, and the pty
        // raises SIGWINCH so the *program* reflows too. Doing only the first is
        // what makes a full-screen app look frozen after a resize.
        if let Some(process) = self.process.as_ref() {
            process.resize(cols, rows);
        }
        self.dirty = true;
    }

    /// Run the child's output through the emulator and paint the result.
    fn pump(&mut self) {
        let Some(process) = self.process.as_mut() else {
            return;
        };
        let mut ended = false;
        if let Some(receiver) = process.output.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(bytes) => {
                        if let Some(terminal) = self.terminal.as_mut() {
                            terminal.write(&bytes);
                        }
                        self.dirty = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        ended = true;
                        break;
                    }
                }
            }
        }
        if ended {
            process.output = None;
            self.dirty = true;
        }
        if std::mem::take(&mut self.dirty) {
            self.draw();
        }
    }

    fn draw(&mut self) {
        let (Some(surface), Some(terminal), Some(window), Some(regular)) = (
            self.surface.as_mut(),
            self.terminal.as_mut(),
            self.window.as_ref(),
            self.font.as_ref(),
        ) else {
            return;
        };
        let bold = self.font_bold.as_ref();
        let Ok(snapshot) = terminal.snapshot() else {
            return;
        };
        let size = window.inner_size();
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        let width = size.width as usize;

        // Cells are whole device pixels so a column is never half a pixel wide:
        // a fractional advance would make every other column drift, which is
        // the one thing a terminal must never do.
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        let cell_w = (self.cell_w * scale).round().max(1.0) as u32;
        let cell_h = (self.cell_h * scale).round().max(1.0) as u32;
        let px_size = FONT_SIZE * scale;
        let ascent = regular
            .horizontal_line_metrics(px_size)
            .map(|metrics| metrics.ascent)
            .unwrap_or(px_size * 0.8);
        let underline_y = (ascent + 1.0).max(0.0);

        // A terminal is a program's canvas, not a themed surface: black ground,
        // and the palette is whatever the program asks for.
        buffer.fill(0x0000_0000);

        for (index, cell) in snapshot.cells.iter().enumerate() {
            let x = (index % usize::from(snapshot.cols)) as u32;
            let y = (index / usize::from(snapshot.cols)) as u32;
            let (px, py) = (x * cell_w, y * cell_h);
            if px + cell_w > size.width || py + cell_h > size.height {
                continue;
            }
            let (fg, bg) = resolve(cell);
            let (fr, fg_g, fb) = unpack(fg);
            for dy in 0..cell_h {
                for dx in 0..cell_w {
                    buffer[(py + dy) as usize * width + (px + dx) as usize] = bg;
                }
            }
            if cell.text.is_empty() {
                continue;
            }
            let face = if cell.bold { bold.unwrap_or(regular) } else { regular };
            let mut advance = 0.0f32;
            for ch in cell.text.chars() {
                if ch == '\u{200b}' {
                    continue;
                }
                let (metrics, coverage) = face.rasterize(ch, px_size);
                let base_y = py as f32 + ascent;
                let origin_x = px as f32 + advance + metrics.xmin as f32;
                let origin_y = base_y - metrics.ymin as f32 - metrics.height as f32;
                for (row, line) in coverage.chunks(metrics.width.max(1)).enumerate() {
                    for (col, alpha) in line.iter().enumerate() {
                        if *alpha == 0 {
                            continue;
                        }
                        let tx = origin_x as i32 + col as i32;
                        let ty = origin_y as i32 + row as i32;
                        if tx < 0 || ty < 0 || tx >= size.width as i32 || ty >= size.height as i32 {
                            continue;
                        }
                        let pixel = &mut buffer[ty as usize * width + tx as usize];
                        blend(pixel, (fr, fg_g, fb), *alpha);
                    }
                }
                advance += metrics.advance_width;
            }
            if cell.underline {
                // The rule belongs to the cell, so it spans the full cell width
                // rather than only the glyph: underlined blanks stay visible.
                let top = py as f32 + underline_y;
                let thickness = (scale.ceil() as u32).max(1);
                for dy in 0..thickness {
                    let y = (top as u32) + dy;
                    if y >= size.height {
                        break;
                    }
                    for dx in 0..cell_w {
                        let x = px + dx;
                        if x >= size.width {
                            break;
                        }
                        let pixel = &mut buffer[y as usize * width + x as usize];
                        *pixel = fg;
                    }
                }
            }
        }

        // A block caret, filled and then the character punched out on top: it
        // has to be visible without hiding what is under it.
        if let Some((cx, cy)) = snapshot.cursor {
            let (px, py) = (u32::from(cx) * cell_w, u32::from(cy) * cell_h);
            let caret = 0x00d0_d0d0;
            for dy in 0..cell_h {
                for dx in 0..cell_w {
                    let (x, y) = (px + dx, py + dy);
                    if x < size.width && y < size.height {
                        buffer[y as usize * width + x as usize] = caret;
                    }
                }
            }
            if let Some(cell) = snapshot.cell(cx, cy)
                && let Some(ch) = cell.text.chars().next()
            {
                let (metrics, coverage) = regular.rasterize(ch, px_size);
                let base_y = py as f32 + ascent;
                let origin_x = px as f32 + metrics.xmin as f32;
                let origin_y = base_y - metrics.ymin as f32 - metrics.height as f32;
                for (row, line) in coverage.chunks(metrics.width.max(1)).enumerate() {
                    for (col, alpha) in line.iter().enumerate() {
                        if *alpha == 0 {
                            continue;
                        }
                        let tx = origin_x as i32 + col as i32;
                        let ty = origin_y as i32 + row as i32;
                        if tx < 0 || ty < 0 || tx >= size.width as i32 || ty >= size.height as i32 {
                            continue;
                        }
                        let pixel = &mut buffer[ty as usize * width + tx as usize];
                        blend(pixel, (0, 0, 0), *alpha);
                    }
                }
            }
        }

        let _ = buffer.present();
    }
}

fn unpack(color: u32) -> (u8, u8, u8) {
    (
        (color >> 16 & 0xff) as u8,
        (color >> 8 & 0xff) as u8,
        (color & 0xff) as u8,
    )
}

/// Alpha-blend one coverage sample of `fg` into a packed pixel.
fn blend(pixel: &mut u32, fg: (u8, u8, u8), alpha: u8) {
    let (br, bg, bb) = unpack(*pixel);
    let a = u32::from(alpha);
    let inv = 255 - a;
    let mix = |back: u8, front: u8| -> u32 {
        (u32::from(back) * inv + u32::from(front) * a) / 255
    };
    *pixel = (mix(br, fg.0) << 16) | (mix(bg, fg.1) << 8) | mix(bb, fg.2);
}

/// A cell's colours as ARGB for `softbuffer`. The terminal's explicit colour
/// wins; otherwise the theme's default pair, which keeps a terminal themed
/// while still honouring a program that asked for something specific.
fn resolve(cell: &soil_vt::Cell) -> (u32, u32) {
    let default_fg = 0x00d0_d0d0;
    let default_bg = 0x0000_0000;
    let (mut fg, mut bg) = match (cell.fg, cell.bg) {
        (Some(fg), Some(bg)) => (rgb(fg), rgb(bg)),
        (Some(fg), None) => (rgb(fg), default_bg),
        (None, Some(bg)) => (default_fg, rgb(bg)),
        (None, None) => (default_fg, default_bg),
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
    let r = (color >> 16 & 0xff) / 2;
    let g = (color >> 8 & 0xff) / 2;
    let b = (color & 0xff) / 2;
    (r << 16) | (g << 8) | b
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let title = self
            .options
            .title
            .clone()
            .unwrap_or_else(|| "Soil terminal".into());
        let window = std::sync::Arc::new(
            event_loop
            .create_window(
                Window::default_attributes()
                    .with_title(title)
                    .with_inner_size(LogicalSize::new(960.0, 600.0)),
            )
            .expect("window"),
        );
        let context = softbuffer::Context::new(event_loop.owned_display_handle())
            .expect("softbuffer context");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.scale = window.scale_factor() as f32;
        if self.font.is_none() {
            let (font, bold, cell_w, cell_h) = load_font();
            self.font = Some(font);
            self.font_bold = bold;
            self.cell_w = cell_w;
            self.cell_h = cell_h;
        }
        let (cols, rows) = self.grid_size(window.inner_size());
        self.terminal = Some(Session::new(cols, rows, 10_000).expect("terminal"));

        if let Some(path) = self.options.text.clone() {
            // A diagnostic, not a session: the text goes through the emulator so
            // escape sequences in a traceback are interpreted the same way a
            // program's output would be, and there is nothing to type into.
            let bytes = if path == "-" {
                let mut buffer = Vec::new();
                let _ = std::io::stdin().read_to_end(&mut buffer);
                buffer
            } else {
                std::fs::read(&path).unwrap_or_else(|error| {
                    format!("soil-terminal: cannot read {path}: {error}\r\n").into_bytes()
                })
            };
            if let Some(terminal) = self.terminal.as_mut() {
                terminal.write(&bytes);
            }
        } else {
            // Any program works; a real terminal takes the user's shell, and
            // this one does the same so the window is useful rather than a demo.
            let command = if self.options.command.is_empty() {
                vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
            } else {
                self.options.command.clone()
            };
            let (cols, rows) = self.grid_size(window.inner_size());
            self.process = Some(Terminal::spawn(&command, cols, rows));
        }
        self.context = Some(context);
        self.surface = Some(surface);
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.kill();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let (Some(surface), Some(window)) = (self.surface.as_mut(), self.window.as_ref())
                {
                    let _ = surface.resize(
                        std::num::NonZeroU32::new(size.width.max(1)).unwrap(),
                        std::num::NonZeroU32::new(size.height.max(1)).unwrap(),
                    );
                    let _ = window;
                }
                self.resize_terminal(size);
                self.dirty = true;
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(window) = self.window.as_ref() {
                    self.scale = window.scale_factor() as f32;
                    let size = window.inner_size();
                    self.resize_terminal(size);
                    self.dirty = true;
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let bytes = encode::key(&event.logical_key, self.modifiers);
                if !bytes.is_empty() {
                    self.send(&bytes);
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.pump();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

fn main() {
    let options = parse();
    let event_loop = EventLoop::new().expect("event loop");
    // Redraw on demand: a terminal that repaints 60 times a second for a
    // blinking cursor burns a laptop battery for nothing.
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        options,
        ..App::default()
    };
    event_loop.run_app(&mut app).expect("run");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diagnostic_goes_through_the_emulator() {
        // The point of `--text` is that error output is *terminal* content: a
        // colour sequence in a traceback is interpreted, not printed literally.
        let mut terminal = Session::new(40, 6, 100).expect("terminal");
        terminal.write(b"\x1b[31mfailed\x1b[0m to open field-notes\r\n");
        let snapshot = terminal.snapshot().unwrap();
        assert_eq!(snapshot.row_text(0), "failed to open field-notes");
        assert!(snapshot.cell(0, 0).unwrap().fg.is_some());
    }
}

#[cfg(test)]
mod render_tests {
    use soil_vt::Session;

    /// Colour and weight are carried per cell by the emulator, so the renderer
    /// only has to honour them. This pins the emulator half of that contract:
    /// 24-bit colour survives, and bold is an attribute rather than an effect
    /// the drawing code has to guess.
    #[test]
    fn truecolor_and_bold_arrive_per_cell() {
        let mut terminal = Session::new(40, 4, 10).expect("terminal");
        terminal.write(b"\x1b[1m\x1b[38;2;12;200;240mtrue\x1b[0m plain");
        let snapshot = terminal.snapshot().unwrap();
        let coloured = snapshot.cell(0, 0).unwrap();
        assert!(coloured.bold, "bold must be an attribute");
        let fg = coloured.fg.expect("24-bit foreground");
        assert_eq!((fg.r, fg.g, fg.b), (12, 200, 240));
        assert!(!snapshot.cell(5, 0).unwrap().bold, "reset ends the run");
    }

    #[test]
    fn underline_is_a_cell_attribute() {
        let mut terminal = Session::new(20, 2, 10).expect("terminal");
        terminal.write(b"\x1b[4munder\x1b[0m");
        let snapshot = terminal.snapshot().unwrap();
        assert!(snapshot.cell(0, 0).unwrap().underline);
        assert!(!snapshot.cell(0, 0).unwrap().bold);
    }
}
