//! The host end of a real terminal: a PTY, a shell, and soil-vt.
//!
//! The Spore never sees a file descriptor or a process. It asks for a
//! snapshot, renders the cells, and sends keys back; this type owns the rest.

use std::io::{Read, Write};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use soil_webgpu::{TerminalCell, TerminalHost, TerminalSnapshot};

struct State {
    host: soil_vt::Host,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    _reader: std::thread::JoinHandle<()>,
}

pub struct PtyTerminal {
    cols: u16,
    rows: u16,
    /// The grid generation the last snapshot reported, so replies the emulator
    /// queued while parsing that output go back to the program once.
    seen: u64,
    state: Option<State>,
}

impl PtyTerminal {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            seen: 0,
            state: None,
        }
    }

    fn ensure(&mut self) -> Result<&mut State, String> {
        if self.state.is_none() {
            self.state = Some(State::spawn(self.cols, self.rows)?);
        }
        Ok(self
            .state
            .as_mut()
            .expect("terminal state was just created"))
    }
}

impl State {
    fn spawn(cols: u16, rows: u16) -> Result<Self, String> {
        let host = soil_vt::Host::spawn(cols, rows, 10_000)
            .map_err(|error| format!("soil-vt: {error:?}"))?;
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("openpty: {error}"))?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
        let mut command = CommandBuilder::new(shell);
        command.arg("-l");
        command.env("TERM", "xterm-256color");
        if let Some(home) = std::env::var_os("HOME") {
            command.cwd(home);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("spawn shell: {error}"))?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("pty reader: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("pty writer: {error}"))?;
        let emulator = host.clone();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => emulator.write(&buffer[..read]),
                }
            }
        });
        Ok(Self {
            host,
            master: pair.master,
            writer,
            child,
            _reader: reader,
        })
    }
}

impl TerminalHost for PtyTerminal {
    fn snapshot(&mut self) -> Result<TerminalSnapshot, String> {
        let seen = self.seen;
        let state = self.ensure()?;
        let snapshot = state.host.snapshot();
        let generation = state.host.generation();
        if generation != seen && generation != 0 {
            // Device status reports and mode queries are answers, not output.
            // A program that asked one waits for it, so they go straight back
            // down the same pty the program is reading.
            let responses = state.host.drain_responses();
            if !responses.is_empty() {
                let _ = state.writer.write_all(&responses);
                let _ = state.writer.flush();
            }
        }
        self.seen = generation;
        Ok(TerminalSnapshot {
            cols: u32::from(snapshot.cols),
            rows: u32::from(snapshot.rows),
            cells: snapshot
                .cells
                .iter()
                .map(|cell| TerminalCell {
                    text: cell.text.clone(),
                    fg: cell.fg.map(|color| {
                        (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
                    }),
                    bg: cell.bg.map(|color| {
                        (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
                    }),
                    bold: cell.bold,
                    italic: cell.italic,
                    underline: cell.underline,
                    faint: cell.faint,
                    inverse: cell.inverse,
                    wide_tail: cell.wide_tail,
                })
                .collect(),
            cursor: snapshot
                .cursor
                .map(|(row, col)| (u32::from(row), u32::from(col))),
            title: snapshot.title.clone(),
            generation,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        let state = self.ensure()?;
        state
            .writer
            .write_all(bytes)
            .and_then(|_| state.writer.flush())
            .map_err(|error| format!("pty write: {error}"))
    }

    fn scroll(&mut self, delta: i32) -> Result<(), String> {
        let state = self.ensure()?;
        state.host.scroll(isize::from(delta as i16));
        Ok(())
    }

    fn resize(&mut self, cols: u32, rows: u32) -> Result<(), String> {
        let cols = u16::try_from(cols).map_err(|_| "columns over budget")?;
        let rows = u16::try_from(rows).map_err(|_| "rows over budget")?;
        let state = self.ensure()?;
        state
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("pty resize: {error}"))?;
        state.host.resize(cols, rows);
        self.cols = cols;
        self.rows = rows;
        Ok(())
    }
}

impl Drop for PtyTerminal {
    fn drop(&mut self) {
        if let Some(state) = self.state.as_mut() {
            state.host.shutdown();
            let _ = state.child.kill();
        }
    }
}
