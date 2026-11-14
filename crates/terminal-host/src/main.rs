//! Soil's native Mycelium + PTY host for the terminal Spore.
//!
//! Directory: `mycelium targets --json`, falling back to `mycelium fleet
//! status --json` for fleet hosts. Session: a host PTY running
//! `mycelium ssh <hostname>`. The Spore owns VT state and submits a cell grid;
//! this host renders it as ASCII or in a window.
//!
//! The host lives in Soil because `soil:mycelium` and the terminal surface are
//! host contracts, not Spore code. The Spore package only names this binary as
//! its `native` launch host.
//!
//! `TERMINAL_MOCK=1` keeps the deterministic fixture for tests.
//! `TERMINAL_SHELL` overrides the spawn command (development only).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::Value;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};

mod window;

wasmtime::component::bindgen!({ path: "wit", world: "terminal-spore" });

use soil::mycelium::{directory, session};
use soil::terminal::surface;

/// A deterministic ANSI frame for the emulator fixture. It used to live in
/// the Spore's `terminal-core`; the host only needs the bytes, not the guest
/// grid types.
const ANSI_FIXTURE: &str = "\x1b]2;terminal fixture\x07\
\x1b[1;32mhello\x1b[0m from \x1b[1;34mmycelium\x1b[0m\r\n\
plain\r\n\
\x1b[7m reversed \x1b[0m \x1b[4munderline\x1b[0m\r\n\
\x1b[2J\x1b[H\
\x1b[33mstatus:\x1b[0m ready";

const MAX_READ: u32 = 64 * 1024;
const MAX_FRAME_CELLS: usize = 4096;
const MAX_TARGETS: usize = 256;

struct State {
    targets: Vec<directory::Target>,
    sessions: BTreeMap<u64, Session>,
    views: BTreeMap<u64, MockView>,
    next_handle: u64,
    last_session: Option<u64>,
    mock: bool,
    wasi: wasmtime_wasi::WasiCtx,
    table: wasmtime_wasi::ResourceTable,
}

enum Session {
    Mock {
        target: String,
        bytes: Vec<u8>,
        position: usize,
        closed: bool,
        cols: u32,
        rows: u32,
    },
    Pty(PtySession),
}

struct PtySession {
    target: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
    eof: bool,
    cols: u32,
    rows: u32,
}

struct MockView {
    cols: u32,
    rows: u32,
    generation: u64,
    focused: bool,
    visible: bool,
}

impl wasmtime_wasi::WasiView for State {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl State {
    fn load() -> Result<Self> {
        let mock = std::env::var("TERMINAL_MOCK").is_ok_and(|value| value == "1");
        let targets = if mock {
            fixture_targets()
        } else {
            load_mycelium_targets().context("load Mycelium directory")?
        };
        if targets.is_empty() {
            bail!(
                "Mycelium returned no targets; try `mycelium targets` or `mycelium fleet status`"
            );
        }
        Ok(Self {
            targets,
            sessions: BTreeMap::new(),
            views: BTreeMap::new(),
            next_handle: 1,
            last_session: None,
            mock,
            wasi: wasmtime_wasi::WasiCtxBuilder::new().inherit_stdio().build(),
            table: wasmtime_wasi::ResourceTable::new(),
        })
    }

    fn handle(&mut self) -> u64 {
        let handle = self.next_handle;
        self.next_handle = self.next_handle.saturating_add(1);
        handle
    }

    fn target(&self, id: &str) -> Option<&directory::Target> {
        self.targets.iter().find(|target| target.id == id)
    }

    fn open_pty(&mut self, target: &directory::Target, cols: u32, rows: u32) -> Result<PtySession> {
        let host = target
            .id
            .strip_prefix("mycelium:")
            .unwrap_or(&target.label)
            .to_string();
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize {
            rows: u16::try_from(rows).unwrap_or(24),
            cols: u16::try_from(cols).unwrap_or(80),
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut command = if let Ok(shell) = std::env::var("TERMINAL_SHELL") {
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(shell);
            cmd
        } else {
            let bin = std::env::var("MYCELIUM_BIN").unwrap_or_else(|_| "mycelium".into());
            let mut cmd = CommandBuilder::new(bin);
            cmd.arg("ssh");
            cmd.arg(&host);
            cmd
        };
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");

        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(PtySession {
            target: host,
            master: pair.master,
            writer,
            child,
            rx,
            eof: false,
            cols,
            rows,
        })
    }

    fn read_mock(
        entry: &mut (String, Vec<u8>, usize, bool, u32, u32),
        max_bytes: u32,
    ) -> Option<Vec<u8>> {
        let (_, bytes, position, closed, _, _) = entry;
        if *closed || *position >= bytes.len() {
            *closed = true;
            return None;
        }
        let max = usize::try_from(max_bytes.min(MAX_READ))
            .unwrap_or(MAX_READ as usize)
            .max(1);
        let end = position.saturating_add(max).min(bytes.len());
        let chunk = bytes[*position..end].to_vec();
        *position = end;
        Some(chunk)
    }
}

impl directory::Host for State {
    fn targets(
        &mut self,
        kind: Option<String>,
        cursor: u64,
        limit: u32,
    ) -> Result<directory::Page, directory::Error> {
        let limit = usize::try_from(limit.min(64)).unwrap_or(64).max(1);
        let start = usize::try_from(cursor).map_err(|_| directory::Error::Invalid)?;
        let filtered: Vec<_> = self
            .targets
            .iter()
            .filter(|target| kind.as_ref().is_none_or(|kind| &target.kind == kind))
            .cloned()
            .collect();
        if start > filtered.len() {
            return Err(directory::Error::Invalid);
        }
        let end = start.saturating_add(limit).min(filtered.len());
        let next = (end < filtered.len()).then_some(end as u64);
        Ok(directory::Page {
            entries: filtered[start..end].to_vec(),
            next,
        })
    }

    fn get(&mut self, id: String) -> Result<directory::Target, directory::Error> {
        self.target(&id).cloned().ok_or(directory::Error::NotFound)
    }
}

impl session::Host for State {
    fn open_shell(&mut self, target: String, cols: u32, rows: u32) -> Result<u64, session::Error> {
        let entry = self
            .target(&target)
            .cloned()
            .ok_or(session::Error::UnknownTarget)?;
        if !entry.shell {
            return Err(session::Error::Denied);
        }
        if cols == 0 || rows == 0 || cols > 400 || rows > 200 {
            return Err(session::Error::OverBudget);
        }
        let handle = self.handle();
        let session = if self.mock {
            Session::Mock {
                target,
                bytes: ANSI_FIXTURE.as_bytes().to_vec(),
                position: 0,
                closed: false,
                cols,
                rows,
            }
        } else {
            Session::Pty(
                self.open_pty(&entry, cols, rows)
                    .map_err(|_| session::Error::Unreachable)?,
            )
        };
        self.sessions.insert(handle, session);
        self.last_session = Some(handle);
        Ok(handle)
    }

    fn read(
        &mut self,
        session: u64,
        max_bytes: u32,
        timeout_ms: u32,
    ) -> Result<Option<Vec<u8>>, session::Error> {
        let entry = self
            .sessions
            .get_mut(&session)
            .ok_or(session::Error::Closed)?;
        match entry {
            Session::Mock {
                target,
                bytes,
                position,
                closed,
                cols,
                rows,
            } => {
                let mut mock = (
                    std::mem::take(target),
                    std::mem::take(bytes),
                    *position,
                    *closed,
                    *cols,
                    *rows,
                );
                let chunk = State::read_mock(&mut mock, max_bytes);
                *target = mock.0;
                *bytes = mock.1;
                *position = mock.2;
                *closed = mock.3;
                *cols = mock.4;
                *rows = mock.5;
                Ok(chunk)
            }
            Session::Pty(entry) => {
                if entry.eof {
                    return Ok(None);
                }
                match entry
                    .rx
                    .recv_timeout(Duration::from_millis(u64::from(timeout_ms)))
                {
                    Ok(chunk) => Ok(Some(chunk)),
                    Err(mpsc::RecvTimeoutError::Timeout) => Ok(Some(Vec::new())),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        entry.eof = true;
                        Ok(None)
                    }
                }
            }
        }
    }

    fn write(&mut self, session: u64, bytes: Vec<u8>) -> Result<u32, session::Error> {
        let entry = self
            .sessions
            .get_mut(&session)
            .ok_or(session::Error::Closed)?;
        match entry {
            Session::Mock { closed, .. } => {
                if *closed {
                    return Err(session::Error::Closed);
                }
                Ok(u32::try_from(bytes.len()).unwrap_or(u32::MAX))
            }
            Session::Pty(entry) => {
                entry
                    .writer
                    .write_all(&bytes)
                    .map_err(|_| session::Error::Io)?;
                entry.writer.flush().map_err(|_| session::Error::Io)?;
                Ok(u32::try_from(bytes.len()).unwrap_or(u32::MAX))
            }
        }
    }

    fn resize(&mut self, session: u64, cols: u32, rows: u32) -> Result<(), session::Error> {
        if cols == 0 || rows == 0 || cols > 400 || rows > 200 {
            return Err(session::Error::OverBudget);
        }
        let entry = self
            .sessions
            .get_mut(&session)
            .ok_or(session::Error::Closed)?;
        match entry {
            Session::Mock {
                cols: width,
                rows: height,
                ..
            } => {
                *width = cols;
                *height = rows;
            }
            Session::Pty(entry) => {
                entry
                    .master
                    .resize(PtySize {
                        rows: u16::try_from(rows).unwrap_or(24),
                        cols: u16::try_from(cols).unwrap_or(80),
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|_| session::Error::Io)?;
                entry.cols = cols;
                entry.rows = rows;
            }
        }
        Ok(())
    }

    fn status(&mut self, session: u64) -> Result<session::State, session::Error> {
        let entry = self
            .sessions
            .get_mut(&session)
            .ok_or(session::Error::Closed)?;
        match entry {
            Session::Mock {
                target,
                closed,
                cols,
                rows,
                ..
            } => Ok(session::State {
                phase: if *closed {
                    session::Phase::Exited
                } else {
                    session::Phase::Connected
                },
                exit_code: Some(0),
                message: Some(format!("mock PTY for {target} ({cols}x{rows})")),
            }),
            Session::Pty(entry) => {
                let (phase, code, message) = match entry.child.try_wait() {
                    Ok(Some(status)) => (
                        session::Phase::Exited,
                        Some(status.exit_code() as i32),
                        Some(format!("exited {:?}", status)),
                    ),
                    Ok(None) => (
                        session::Phase::Connected,
                        None,
                        Some(format!(
                            "mycelium ssh {} ({}x{})",
                            entry.target, entry.cols, entry.rows
                        )),
                    ),
                    Err(error) => (session::Phase::Failed, None, Some(error.to_string())),
                };
                Ok(session::State {
                    phase,
                    exit_code: code,
                    message,
                })
            }
        }
    }

    fn close(&mut self, session: u64) -> Result<(), session::Error> {
        let entry = self
            .sessions
            .get_mut(&session)
            .ok_or(session::Error::Closed)?;
        match entry {
            Session::Mock { closed, .. } => *closed = true,
            Session::Pty(entry) => {
                let _ = entry.child.kill();
                entry.eof = true;
            }
        }
        Ok(())
    }

    fn exec(
        &mut self,
        target: String,
        argv: Vec<String>,
        _timeout_ms: u32,
    ) -> Result<session::ExecResult, session::Error> {
        let entry = self
            .target(&target)
            .cloned()
            .ok_or(session::Error::UnknownTarget)?;
        if !entry.shell {
            return Err(session::Error::Denied);
        }
        if self.mock {
            let line = format!("mock exec on {target}: {}\n", argv.join(" "));
            return Ok(session::ExecResult {
                code: 0,
                stdout: line.into_bytes(),
                stderr: Vec::new(),
            });
        }
        let host = target
            .strip_prefix("mycelium:")
            .unwrap_or(&entry.label)
            .to_string();
        let bin = std::env::var("MYCELIUM_BIN").unwrap_or_else(|_| "mycelium".into());
        let mut command = Command::new(bin);
        command.arg("exec").arg(host).arg("--").args(&argv);
        let output = command.output().map_err(|_| session::Error::Unreachable)?;
        Ok(session::ExecResult {
            code: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

impl surface::Host for State {
    fn open(&mut self, cols: u32, rows: u32) -> Result<u64, surface::Failure> {
        if cols == 0 || rows == 0 || (cols as usize) * (rows as usize) > MAX_FRAME_CELLS {
            return Err(surface::Failure::OverBudget);
        }
        let handle = self.handle();
        self.views.insert(
            handle,
            MockView {
                cols,
                rows,
                generation: 0,
                focused: true,
                visible: true,
            },
        );
        Ok(handle)
    }

    fn present(
        &mut self,
        view: u64,
        frame: surface::Frame,
    ) -> Result<surface::State, surface::Failure> {
        let entry = self.views.get_mut(&view).ok_or(surface::Failure::Closed)?;
        if frame.cols != entry.cols || frame.rows != entry.rows {
            return Err(surface::Failure::Invalid);
        }
        if frame.cells.len() != (frame.cols as usize) * (frame.rows as usize) {
            return Err(surface::Failure::Invalid);
        }
        if frame.generation < entry.generation {
            return Err(surface::Failure::Stale);
        }
        entry.generation = frame.generation;

        println!(
            "--- frame {}x{} cursor=({}, {}) visible={} title={:?} ---",
            frame.cols,
            frame.rows,
            frame.cursor.row,
            frame.cursor.column,
            frame.cursor.visible,
            frame.title
        );
        for row in 0..frame.rows {
            let mut line = String::new();
            for col in 0..frame.cols {
                let index = (row as usize) * (frame.cols as usize) + (col as usize);
                let cell = &frame.cells[index];
                line.push_str(if cell.text.is_empty() {
                    " "
                } else {
                    &cell.text
                });
            }
            println!("{}", line.trim_end());
        }

        Ok(surface::State {
            generation: entry.generation,
            cols: entry.cols,
            rows: entry.rows,
            focused: entry.focused,
            visible: entry.visible,
        })
    }

    fn next_input(
        &mut self,
        view: u64,
        _timeout_ms: u32,
    ) -> Result<Option<surface::Input>, surface::Failure> {
        if !self.views.contains_key(&view) {
            return Err(surface::Failure::Closed);
        }
        Ok(None)
    }

    fn observe(&mut self, view: u64) -> Result<surface::State, surface::Failure> {
        let entry = self.views.get(&view).ok_or(surface::Failure::Closed)?;
        Ok(surface::State {
            generation: entry.generation,
            cols: entry.cols,
            rows: entry.rows,
            focused: entry.focused,
            visible: entry.visible,
        })
    }

    fn close(&mut self, view: u64) -> Result<(), surface::Failure> {
        self.views
            .remove(&view)
            .map(|_| ())
            .ok_or(surface::Failure::Closed)
    }
}

fn fixture_targets() -> Vec<directory::Target> {
    vec![
        directory::Target {
            id: "mycelium:agora-one".into(),
            label: "agora-one".into(),
            kind: "host".into(),
            site: Some("agora".into()),
            health: "healthy".into(),
            shell: true,
        },
        directory::Target {
            id: "mycelium:pris".into(),
            label: "pris".into(),
            kind: "host".into(),
            site: Some("pris".into()),
            health: "healthy".into(),
            shell: true,
        },
        directory::Target {
            id: "mycelium:studio".into(),
            label: "studio".into(),
            kind: "host".into(),
            site: Some("studio".into()),
            health: "degraded".into(),
            shell: false,
        },
        directory::Target {
            id: "mycelium:home-pi".into(),
            label: "home-pi".into(),
            kind: "service".into(),
            site: Some("home".into()),
            health: "healthy".into(),
            shell: false,
        },
    ]
}

fn load_mycelium_targets() -> Result<Vec<directory::Target>> {
    let bin = std::env::var("MYCELIUM_BIN").unwrap_or_else(|_| "mycelium".into());
    let mut targets = parse_targets(&run_json(&bin, &["targets", "--json"])?);
    if targets.is_empty() {
        targets = parse_fleet(&run_json(&bin, &["fleet", "status", "--json"])?);
    }
    targets.truncate(MAX_TARGETS);
    Ok(targets)
}

fn run_json(bin: &str, args: &[&str]) -> Result<Value> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("run {bin} {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "{bin} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("parse Mycelium JSON")
}

fn parse_targets(value: &Value) -> Vec<directory::Target> {
    let entries = value.as_array().cloned().unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|entry| {
            let id = entry
                .get("id")
                .or_else(|| entry.get("device_id"))
                .and_then(Value::as_str)?;
            let label = entry
                .get("name")
                .or_else(|| entry.get("label"))
                .or_else(|| entry.get("hostname"))
                .and_then(Value::as_str)
                .unwrap_or(id);
            Some(directory::Target {
                id: format!("mycelium:{id}"),
                label: label.to_string(),
                kind: entry
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("host")
                    .to_string(),
                site: entry
                    .get("site")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                health: entry
                    .get("health")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                shell: true,
            })
        })
        .collect()
}

fn parse_fleet(value: &Value) -> Vec<directory::Target> {
    let entries = value.as_array().cloned().unwrap_or_default();
    let mut seen = BTreeMap::new();
    for entry in entries {
        let Some(hostname) = entry.get("hostname").and_then(Value::as_str) else {
            continue;
        };
        if hostname.is_empty() {
            continue;
        }
        let ssh = entry
            .get("ssh_listening")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let site = entry
            .get("site")
            .and_then(Value::as_str)
            .map(str::to_string);
        let health = if ssh { "healthy" } else { "degraded" };
        seen.entry(hostname.to_string())
            .or_insert(directory::Target {
                id: format!("mycelium:{hostname}"),
                label: hostname.to_string(),
                kind: "host".into(),
                site,
                health: health.into(),
                shell: true,
            });
    }
    seen.into_values().collect()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let guest_args = if args.is_empty() {
        vec!["open-first".to_string()]
    } else {
        args
    };
    let module = std::env::var("TERMINAL_SPORE")
        .or_else(|_| std::env::var("SOIL_SPORE_MODULE"))
        .unwrap_or_else(|_| "spores/terminal/module.wasm".to_string());

    let engine = Engine::default();
    let component = Component::from_file(&engine, &module)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("load component {module}"))?;

    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync::<State>(&mut linker)
        .map_err(anyhow::Error::from)
        .context("link wasi")?;
    directory::add_to_linker::<State, HasSelf<State>>(&mut linker, |state| state)
        .map_err(anyhow::Error::from)
        .context("link soil:mycelium/directory")?;
    session::add_to_linker::<State, HasSelf<State>>(&mut linker, |state| state)
        .map_err(anyhow::Error::from)
        .context("link soil:mycelium/session")?;
    surface::add_to_linker::<State, HasSelf<State>>(&mut linker, |state| state)
        .map_err(anyhow::Error::from)
        .context("link soil:terminal/surface")?;

    let mut store = Store::new(&engine, State::load()?);
    let bindings = TerminalSpore::instantiate(&mut store, &component, &linker)
        .map_err(anyhow::Error::from)
        .context("instantiate terminal Spore")?;

    match bindings.call_run(&mut store, &guest_args) {
        Ok(Ok(())) => {}
        Ok(Err(message)) => bail!("guest failed: {message}"),
        Err(error) => return Err(anyhow!("guest trapped: {error:#}")),
    }

    let snapshot = bindings
        .call_snapshot(&mut store)
        .map_err(anyhow::Error::from)?
        .map_err(|message| anyhow!("snapshot: {message}"))?;
    println!("{}", String::from_utf8_lossy(&snapshot));

    if std::env::var("TERMINAL_WINDOW").is_ok_and(|value| value == "1") {
        window::run(&mut store, &bindings, &snapshot)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn directory_entries_become_targets() {
        let targets = parse_targets(&json!([
            {
                "id": "agora-one",
                "name": "Agora One",
                "kind": "vm",
                "site": "agora",
                "health": "healthy"
            },
            {"device_id": "pris"},
            {"name": "no id"},
        ]));

        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].id, "mycelium:agora-one");
        assert_eq!(targets[0].label, "Agora One");
        assert_eq!(targets[0].kind, "vm");
        assert_eq!(targets[0].site.as_deref(), Some("agora"));
        assert_eq!(targets[1].id, "mycelium:pris");
        assert_eq!(targets[1].label, "pris");
        assert_eq!(targets[1].kind, "host");
        assert_eq!(targets[1].health, "unknown");
    }

    #[test]
    fn fleet_entries_dedupe_and_follow_ssh_state() {
        let targets = parse_fleet(&json!([
            {"hostname": "agora-one", "ssh_listening": true, "site": "agora"},
            {"hostname": "agora-one", "ssh_listening": false},
            {"hostname": "home-pi"},
            {"ssh_listening": true},
        ]));

        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].label, "agora-one");
        assert_eq!(targets[0].health, "healthy");
        assert!(targets[0].shell);
        assert_eq!(targets[1].label, "home-pi");
        assert_eq!(targets[1].health, "degraded");
    }
}
