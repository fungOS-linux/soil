//! A terminal in a native window.
//!
//! The same `soil-vt` grid Canvas draws, but presented by Soil itself: this is
//! the "one Spore, a different host" claim made concrete. The compositor is not
//! involved, so this binary is also the honest test of whether terminal
//! emulation and the process plumbing stand on their own.

mod encode;

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

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
const FONT_CANDIDATES: &[&str] = &[
    "Library/Fonts/Fira Code Regular Nerd Font Complete Mono.otf",
    "Library/Fonts/FiraCodeNerdFontMono-Regular.ttf",
    "Library/Fonts/Fira Code Regular Nerd Font Complete.otf",
    "Library/Fonts/FiraCode-Regular.ttf",
    ".local/share/fonts/FiraCodeNerdFontMono-Regular.ttf",
    "usr/share/fonts/truetype/firacode/FiraCode-Regular.ttf",
];

fn load_font() -> (Font, f32, f32) {
    let mut bytes = None;
    if let Some(path) = std::env::var_os("SOIL_TERMINAL_FONT") {
        bytes = std::fs::read(path).ok();
    }
    if bytes.is_none() {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        for candidate in FONT_CANDIDATES {
            let path = match (&home, candidate.starts_with("Library") || candidate.starts_with('.'))
            {
                (Some(home), true) => home.join(candidate),
                _ => std::path::PathBuf::from("/").join(candidate),
            };
            if let Ok(read) = std::fs::read(&path) {
                bytes = Some(read);
                break;
            }
        }
    }
    let font = bytes
        .and_then(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok())
        .expect("a terminal needs a font: set SOIL_TERMINAL_FONT to a .ttf or .otf");
    // Cell geometry comes from the font: advance is the column width, and the
    // line box is ascent + descent, so a row is as tall as the face says.
    let probe = font.metrics('M', FONT_SIZE);
    let advance = probe.advance_width.max(1.0).round();
    let height = (probe.height as f32).max(FONT_SIZE).ceil();
    (font, advance, height)
}

/// A program's output, tagged so the UI thread can tell "more bytes" from "the
/// program exited" without polling.
enum Output {
    Bytes(Vec<u8>),
    Ended,
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

#[derive(Default)]
struct App {
    options: Options,
    /// Backing-store scale, so a 15pt cell is 15pt on a Retina display too.
    scale: f32,
    window: Option<std::sync::Arc<Window>>,
    context: Option<softbuffer::Context<winit::event_loop::OwnedDisplayHandle>>,
    surface: Option<
        softbuffer::Surface<winit::event_loop::OwnedDisplayHandle, std::sync::Arc<Window>>,
    >,
    terminal: Option<Session>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    output: Option<Receiver<Output>>,
    modifiers: ModifiersState,
    dirty: bool,
    font: Option<Font>,
    cell_w: f32,
    cell_h: f32,
}

fn spawn(command: &[String]) -> (Child, ChildStdin, Receiver<Output>) {
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("TERM", "xterm-256color");
    let mut child = process.spawn().expect("spawn the guest program");
    let mut stdin = child.stdin.take().expect("guest stdin");
    let mut stdout = child.stdout.take().expect("guest stdout");
    let mut stderr = child.stderr.take().expect("guest stderr");
    let (sender, receiver) = channel();
    // Output and exit are two channels' worth of information but one stream to
    // the terminal; both writers share a sender so ordering is preserved.
    let out_sender: Sender<Output> = sender.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_sender.send(Output::Bytes(buffer[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = out_sender.send(Output::Ended);
    });
    let err_sender = sender;
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buffer) {
            if n == 0 || err_sender.send(Output::Bytes(buffer[..n].to_vec())).is_err() {
                return;
            }
        }
    });
    // A closed stdin must not take the process down with it.
    let _ = stdin.flush();
    (child, stdin, receiver)
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
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(bytes);
            let _ = stdin.flush();
        }
    }

    fn resize_terminal(&mut self, size: PhysicalSize<u32>) {
        let (cols, rows) = self.grid_size(size);
        if let Some(terminal) = self.terminal.as_mut()
            && (terminal.cols() != cols || terminal.rows() != rows)
        {
            let _ = terminal.resize(cols, rows);
            self.dirty = true;
        }
    }

    /// Run the child's output through the emulator and paint the result.
    fn pump(&mut self) {
        let Some(receiver) = self.output.as_ref() else {
            return;
        };
        let mut ended = false;
        for message in receiver.try_iter() {
            match message {
                Output::Bytes(bytes) => {
                    if let Some(terminal) = self.terminal.as_mut() {
                        terminal.write(&bytes);
                    }
                    self.dirty = true;
                }
                Output::Ended => ended = true,
            }
        }
        if ended {
            self.output = None;
            self.dirty = true;
        }
        if std::mem::take(&mut self.dirty) {
            self.draw();
        }
    }

    fn draw(&mut self) {
        let (Some(surface), Some(terminal), Some(window), Some(font)) = (
            self.surface.as_mut(),
            self.terminal.as_mut(),
            self.window.as_ref(),
            self.font.as_ref(),
        ) else {
            return;
        };
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
        let ascent = font
            .horizontal_line_metrics(px_size)
            .map(|metrics| metrics.ascent)
            .unwrap_or(px_size * 0.8);

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
            let mut advance = 0.0f32;
            for ch in cell.text.chars() {
                if ch == '\u{200b}' {
                    continue;
                }
                let (metrics, coverage) = font.rasterize(ch, px_size);
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
                let (metrics, coverage) = font.rasterize(ch, px_size);
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
            let (font, cell_w, cell_h) = load_font();
            self.font = Some(font);
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
            let (child, stdin, output) = spawn(&command);
            self.child = Some(child);
            self.stdin = Some(stdin);
            self.output = Some(output);
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
                if let Some(child) = self.child.as_mut() {
                    let _ = child.kill();
                }
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
