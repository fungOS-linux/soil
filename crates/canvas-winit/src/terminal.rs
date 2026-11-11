//! The host end of a real terminal: a PTY, a shell, and soil-vt.
//!
//! The Spore never sees a file descriptor or a process. It asks for a
//! snapshot, renders the cells, and sends keys back; this type owns the rest.

use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use soil_webgpu::{TerminalCell, TerminalHost, TerminalSession, TerminalSnapshot};

struct State {
    host: soil_vt::Host,
    master: Box<dyn MasterPty + Send>,
    /// Shared with the reader thread, which writes the emulator's answers back
    /// as soon as the program's line has been parsed.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    _reader: std::thread::JoinHandle<()>,
}

pub struct PtyTerminal {
    cols: u16,
    rows: u16,
    /// What runs on the pty. `None` is a login shell; a Spore that wants a
    /// different shell (mush) names the command line and the host runs it the
    /// same way, with the same grid contract.
    command: Option<String>,
    state: Option<State>,
}

impl PtyTerminal {
    pub fn new(cols: u16, rows: u16, command: Option<String>) -> Self {
        Self {
            cols,
            rows,
            command,
            state: None,
        }
    }

    fn ensure(&mut self) -> Result<&mut State, String> {
        if self.state.is_none() {
            self.state = Some(State::spawn(self.cols, self.rows, self.command.as_deref())?);
        }
        Ok(self
            .state
            .as_mut()
            .expect("terminal state was just created"))
    }
}

impl State {
    fn spawn(cols: u16, rows: u16, command: Option<&str>) -> Result<Self, String> {
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
        // A named command line runs through `sh -c`, which is one rule for a
        // program with flags and one for a program without.
        let mut builder = match command {
            Some(line) => {
                let mut builder = CommandBuilder::new("/bin/sh");
                builder.arg("-c");
                builder.arg(line);
                builder
            }
            None => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
                let mut builder = CommandBuilder::new(shell);
                builder.arg("-l");
                builder
            }
        };
        builder.env("TERM", "xterm-256color");
        if let Some(home) = std::env::var_os("HOME") {
            builder.cwd(home);
        }
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|error| format!("spawn shell: {error}"))?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("pty reader: {error}"))?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|error| format!("pty writer: {error}"))?,
        ));
        let replies = Arc::clone(&writer);
        let emulator = host.clone();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        emulator.write(&buffer[..read]);
                        // A reply answers a question the program asked, so it
                        // goes back before the program's next line is read:
                        // delaying it to the next frame lands it after the
                        // prompt and the shell echoes it as text.
                        let answers = emulator.drain_responses();
                        if !answers.is_empty()
                            && let Ok(mut replies) = replies.lock()
                        {
                            let _ = replies.write_all(&answers);
                            let _ = replies.flush();
                        }
                    }
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

/// One process: a pty, a shell on it, and the emulator that parses what the
/// shell writes.
struct Session {
    id: u32,
    label: String,
    terminal: PtyTerminal,
}

/// Every shell the terminal Spore is showing. The Spore sees a tab strip; the
/// host owns the processes, and the contract only says which one is active.
pub struct Terminals {
    sessions: Vec<Session>,
    active: Option<u32>,
    next_id: u32,
    default_command: Option<String>,
    cols: u16,
    rows: u16,
}

impl Terminals {
    pub fn new(cols: u16, rows: u16, default_command: Option<String>) -> Self {
        Self {
            sessions: Vec::new(),
            active: None,
            next_id: 1,
            default_command,
            cols,
            rows,
        }
    }

    fn active_session(&mut self) -> Result<&mut Session, String> {
        let id = self
            .active
            .ok_or_else(|| "no terminal session attached".to_owned())?;
        self.sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or_else(|| "the active session is gone".to_owned())
    }
}

impl TerminalHost for Terminals {
    fn sessions(&mut self) -> Result<Vec<TerminalSession>, String> {
        Ok(self
            .sessions
            .iter_mut()
            .map(|session| {
                // The shell names itself in its title once it has one; until
                // then the tab says what the session runs.
                let title = session
                    .terminal
                    .snapshot()
                    .ok()
                    .map(|snapshot| snapshot.title)
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| session.label.clone());
                TerminalSession {
                    id: session.id,
                    title,
                    active: self.active == Some(session.id),
                }
            })
            .collect())
    }

    fn open_session(&mut self, command: Option<String>) -> Result<u32, String> {
        // The command line the app was started with is the *first* shell; a
        // second tab is the user asking for another terminal, which is a login
        // shell unless they say otherwise.
        let command = match (command, self.sessions.is_empty()) {
            (Some(command), _) => Some(command),
            (None, true) => self.default_command.clone(),
            (None, false) => None,
        };
        let label = command
            .as_deref()
            .and_then(|line| line.split_whitespace().next())
            .map(|program| {
                std::path::Path::new(program)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(program)
                    .to_owned()
            })
            .unwrap_or_else(|| {
                std::env::var("SHELL")
                    .ok()
                    .and_then(|shell| {
                        std::path::Path::new(&shell)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "shell".to_owned())
            });
        let id = self.next_id;
        self.next_id += 1;
        self.sessions.push(Session {
            id,
            label,
            terminal: PtyTerminal::new(self.cols, self.rows, command),
        });
        self.active = Some(id);
        Ok(id)
    }

    fn select_session(&mut self, id: u32) -> Result<(), String> {
        if self.sessions.iter().any(|session| session.id == id) {
            self.active = Some(id);
            Ok(())
        } else {
            Err(format!("no session {id}"))
        }
    }

    fn close_session(&mut self, id: u32) -> Result<(), String> {
        let before = self.sessions.len();
        self.sessions.retain(|session| session.id != id);
        if self.sessions.len() == before {
            return Err(format!("no session {id}"));
        }
        if self.active == Some(id) {
            self.active = self.sessions.last().map(|session| session.id);
        }
        Ok(())
    }

    fn snapshot(&mut self) -> Result<TerminalSnapshot, String> {
        self.active_session()?.terminal.snapshot()
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.active_session()?.terminal.write(bytes)
    }

    fn resize(&mut self, cols: u32, rows: u32) -> Result<(), String> {
        self.cols = u16::try_from(cols).unwrap_or(u16::MAX);
        self.rows = u16::try_from(rows).unwrap_or(u16::MAX);
        for session in &mut self.sessions {
            session.terminal.resize(cols, rows)?;
        }
        Ok(())
    }

    fn scroll(&mut self, delta: i32) -> Result<(), String> {
        self.active_session()?.terminal.scroll(delta)
    }

    fn wait(&mut self, after: u64, timeout_ms: u32) -> Result<u64, String> {
        self.active_session()?.terminal.wait(after, timeout_ms)
    }
}

impl TerminalHost for PtyTerminal {
    fn snapshot(&mut self) -> Result<TerminalSnapshot, String> {
        let state = self.ensure()?;
        let snapshot = state.host.snapshot();
        let generation = state.host.generation();
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
            // soil-vt reports (column, row); the boundary type says (row, column).
            cursor: snapshot
                .cursor
                .map(|(column, row)| (u32::from(row), u32::from(column))),
            title: snapshot.title.clone(),
            generation,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        let state = self.ensure()?;
        let mut writer = state.writer.lock().map_err(|_| "pty writer poisoned")?;
        writer
            .write_all(bytes)
            .and_then(|_| writer.flush())
            .map_err(|error| format!("pty write: {error}"))
    }

    fn wait(&mut self, after: u64, timeout_ms: u32) -> Result<u64, String> {
        let state = self.ensure()?;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(u64::from(timeout_ms));
        loop {
            let generation = state.host.generation();
            // A millisecond is below what a person can see and keeps the
            // component thread responsive to a key that arrives mid-wait.
            if generation != after || std::time::Instant::now() >= deadline {
                return Ok(generation);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
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
