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

use font8x8::UnicodeFonts;
use soil_vt::Session;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

/// Logical pixels per cell. 8x8 glyphs at 2x are legible on a normal display
/// and keep the arithmetic exact: cell count is a division, not an estimate.
const CELL: u32 = 16;

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
    fn grid_size(size: PhysicalSize<u32>) -> (u16, u16) {
        (
            (size.width / CELL).max(1) as u16,
            (size.height / CELL).max(1) as u16,
        )
    }

    fn send(&mut self, bytes: &[u8]) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(bytes);
            let _ = stdin.flush();
        }
    }

    fn resize_terminal(&mut self, size: PhysicalSize<u32>) {
        let (cols, rows) = Self::grid_size(size);
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
        let (Some(surface), Some(terminal), Some(window)) = (
            self.surface.as_mut(),
            self.terminal.as_mut(),
            self.window.as_ref(),
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
        // The window is the authority on size; the grid may be one cell behind
        // for a frame, which is better than drawing outside the buffer.
        buffer.fill(0x0010_1014);

        for (index, cell) in snapshot.cells.iter().enumerate() {
            let x = (index % usize::from(snapshot.cols)) as u32;
            let y = (index / usize::from(snapshot.cols)) as u32;
            let (px, py) = (x * CELL, y * CELL);
            if px + CELL > size.width || py + CELL > size.height {
                continue;
            }
            let (fg, bg) = resolve(cell);
            for dy in 0..CELL {
                for dx in 0..CELL {
                    let pixel = &mut buffer[(py + dy) as usize * width + (px + dx) as usize];
                    *pixel = bg;
                }
            }
            let mut drawn = 0u32;
            for ch in cell.text.chars() {
                if ch == ' ' {
                    drawn += 1;
                    continue;
                }
                let glyph = font8x8::BASIC_FONTS.get(ch).or_else(|| font8x8::BASIC_FONTS.get('?'));
                let Some(glyph) = glyph else { continue };
                for (row_index, row) in glyph.iter().enumerate() {
                    for col in 0..8 {
                        if row & (1 << col) == 0 {
                            continue;
                        }
                        // 2x scale: an 8x8 glyph becomes 16x16 and fills the cell.
                        for sy in 0..2 {
                            for sx in 0..2 {
                                let target_y = py + row_index as u32 * 2 + sy;
                                let target_x = px + drawn * CELL + col * 2 + sx;
                                if target_x < size.width && target_y < size.height {
                                    buffer[target_y as usize * width + target_x as usize] = fg;
                                }
                            }
                        }
                    }
                }
                drawn += 1;
            }
        }

        // Cursor last: it is an overlay, not a glyph, so it can invert whatever
        // is already there without re-shaping the cell.
        if let Some((cx, cy)) = snapshot.cursor {
            let (px, py) = (u32::from(cx) * CELL, u32::from(cy) * CELL);
            for dy in 0..CELL {
                for dx in 0..CELL {
                    let (x, y) = (px + dx, py + dy);
                    if x < size.width && y < size.height {
                        let pixel = &mut buffer[y as usize * width + x as usize];
                        *pixel = !*pixel & 0x00ff_ffff;
                    }
                }
            }
        }

        let _ = buffer.present();
    }
}

/// A cell's colours as ARGB for `softbuffer`. The terminal's explicit colour
/// wins; otherwise the theme's default pair, which keeps a terminal themed
/// while still honouring a program that asked for something specific.
fn resolve(cell: &soil_vt::Cell) -> (u32, u32) {
    let default_fg = 0x00e6_e6ea;
    let default_bg = 0x0010_1014;
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
        let (cols, rows) = Self::grid_size(window.inner_size());
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
