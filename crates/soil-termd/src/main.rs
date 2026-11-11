//! `soil-termd`: ptys that outlive the server that serves them.
//!
//! A session is a pty and an emulator. If the web server owns both, restarting
//! the server kills every shell it was showing — including the mush desktops a
//! person left open. This process owns them instead, and any number of servers
//! can come and go against the same socket.
//!
//! Protocol: one JSON object per connection, one JSON object back, then close.
//!
//! ```text
//! {"op":"open","label":"studio","target":"studio","argv":["zsh","-l"],"cols":80,"rows":24}
//! {"op":"list"}
//! {"op":"input","id":1,"data":[101,99,104,111,13]}
//! {"op":"resize","id":1,"cols":120,"rows":40}
//! {"op":"scroll","id":1,"delta":-3}
//! {"op":"snapshot","id":1}
//! {"op":"close","id":1}
//! {"op":"health"}
//! ```
//!
//! `snapshot` answers with the same Lua table `/snapshot.lua` serves, so the
//! renderer does not need to know whether the session is local or remote.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::{Value, json};
use soil_vt::{Host, Snapshot};

const MAX_REQUEST: usize = 1 << 20;
const REAP_AFTER: Duration = Duration::from_secs(30 * 60);

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut socket = default_socket();
    let mut cols = 80u16;
    let mut rows = 24u16;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = PathBuf::from(args.next().context("--socket needs a path")?),
            "--cols" => cols = args.next().context("--cols")?.parse()?,
            "--rows" => rows = args.next().context("--rows")?.parse()?,
            "--help" | "-h" => {
                println!("soil-termd [--socket PATH] [--cols N] [--rows N]");
                return Ok(());
            }
            other => bail!("unknown flag {other}"),
        }
    }

    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // A stale socket from a dead daemon is not a running daemon.
    if socket.exists() {
        if UnixStream::connect(&socket).is_ok() {
            bail!("{} is already served", socket.display());
        }
        let _ = std::fs::remove_file(&socket);
    }

    let listener =
        UnixListener::bind(&socket).with_context(|| format!("bind {}", socket.display()))?;
    println!("soil-termd: {}", socket.display());

    let daemon = Arc::new(Daemon::new(cols, rows));
    let reaper = Arc::clone(&daemon);
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(30));
            reaper.reap();
        }
    });

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || {
            let _ = serve(stream, &daemon);
        });
    }
    Ok(())
}

fn default_socket() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("fungos-term.sock");
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/share/fungos/term.sock")
}

struct Daemon {
    sessions: Mutex<BTreeMap<u64, Arc<Session>>>,
    next: Mutex<u64>,
    cols: u16,
    rows: u16,
}

struct Session {
    id: u64,
    label: String,
    target: String,
    host: Host,
    pty: Pty,
    last_active: Mutex<Instant>,
}

impl Daemon {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            sessions: Mutex::new(BTreeMap::new()),
            next: Mutex::new(1),
            cols,
            rows,
        }
    }

    fn open(&self, label: &str, target: &str, argv: &[String]) -> Result<Arc<Session>> {
        if argv.is_empty() {
            bail!("open needs argv");
        }
        let id = {
            let mut next = self.next.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };
        let host = Host::spawn(self.cols, self.rows, 10_000)?;
        let pty = Pty::spawn(argv, self.cols, self.rows, host.clone())?;
        let session = Arc::new(Session {
            id,
            label: label.to_string(),
            target: target.to_string(),
            host,
            pty,
            last_active: Mutex::new(Instant::now()),
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(id, Arc::clone(&session));
        Ok(session)
    }

    fn get(&self, id: u64) -> Option<Arc<Session>> {
        self.sessions.lock().ok()?.get(&id).cloned()
    }

    fn close(&self, id: u64) -> bool {
        self.sessions
            .lock()
            .map(|mut s| s.remove(&id).is_some())
            .unwrap_or(false)
    }

    /// A session with no traffic for half an hour is a shell nobody is using.
    /// Mush desktops are exempt: they are places, not logins.
    fn reap(&self) {
        let stale: Vec<u64> = self
            .sessions
            .lock()
            .map(|sessions| {
                sessions
                    .values()
                    .filter(|session| {
                        session.target_kind() != "mush"
                            && session
                                .last_active
                                .lock()
                                .map(|t| t.elapsed() > REAP_AFTER)
                                .unwrap_or(false)
                    })
                    .map(|session| session.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in stale {
            self.close(id);
        }
    }
}

impl Session {
    fn target_kind(&self) -> &'static str {
        if self.target.starts_with("mush:") {
            "mush"
        } else {
            "other"
        }
    }

    fn touch(&self) {
        if let Ok(mut last) = self.last_active.lock() {
            *last = Instant::now();
        }
    }

    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "target": self.target,
            "exited": self.pty.exited(),
        })
    }
}

fn serve(mut stream: UnixStream, daemon: &Daemon) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    // One line in, one line out. Reading to EOF would deadlock: the client keeps
    // the connection open waiting for the answer it has not been sent yet.
    let mut request = String::new();
    let mut reader = BufReader::new(stream.try_clone()?);
    reader
        .by_ref()
        .take(MAX_REQUEST as u64)
        .read_line(&mut request)?;

    let value: Value = match serde_json::from_str(request.trim()) {
        Ok(value) => value,
        Err(error) => {
            return reply(
                &mut stream,
                json!({ "error": format!("bad request: {error}") }),
            );
        }
    };
    let op = value.get("op").and_then(Value::as_str).unwrap_or_default();
    let id = value.get("id").and_then(Value::as_u64);

    match op {
        "health" => reply(
            &mut stream,
            json!({
                "ok": true,
                "sessions": daemon.sessions.lock().map(|s| s.len()).unwrap_or(0),
            }),
        ),
        "list" => {
            let sessions: Vec<Value> = daemon
                .sessions
                .lock()
                .map(|sessions| sessions.values().map(|session| session.json()).collect())
                .unwrap_or_default();
            reply(&mut stream, json!({ "sessions": sessions }))
        }
        "open" => {
            let label = value
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("session");
            let target = value.get("target").and_then(Value::as_str).unwrap_or(label);
            let argv: Vec<String> = value
                .get("argv")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            match daemon.open(label, target, &argv) {
                Ok(session) => reply(&mut stream, session.json()),
                Err(error) => reply(&mut stream, json!({ "error": format!("{error:#}") })),
            }
        }
        "input" => match id.and_then(|id| daemon.get(id)) {
            Some(session) => {
                let bytes: Vec<u8> = value
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_u64())
                            .map(|byte| byte as u8)
                            .collect()
                    })
                    .unwrap_or_default();
                session.touch();
                session.host.scroll_to_bottom();
                session.pty.write(&bytes);
                reply(&mut stream, json!({ "ok": true }))
            }
            None => reply(&mut stream, json!({ "error": "no such session" })),
        },
        "resize" => match id.and_then(|id| daemon.get(id)) {
            Some(session) => {
                let cols = value.get("cols").and_then(Value::as_u64).unwrap_or(80) as u16;
                let rows = value.get("rows").and_then(Value::as_u64).unwrap_or(24) as u16;
                session.touch();
                session.host.resize(cols, rows);
                session.pty.resize(cols, rows);
                reply(&mut stream, json!({ "ok": true }))
            }
            None => reply(&mut stream, json!({ "error": "no such session" })),
        },
        "scroll" => match id.and_then(|id| daemon.get(id)) {
            Some(session) => {
                let delta = value.get("delta").and_then(Value::as_i64).unwrap_or(0) as isize;
                session.touch();
                session.host.scroll(delta);
                reply(&mut stream, json!({ "ok": true }))
            }
            None => reply(&mut stream, json!({ "error": "no such session" })),
        },
        "snapshot" => match id.and_then(|id| daemon.get(id)) {
            Some(session) => {
                session.touch();
                let snapshot: std::sync::Arc<Snapshot> = session.host.snapshot();
                reply(&mut stream, json!({ "lua": lua(snapshot.as_ref()) }))
            }
            None => reply(&mut stream, json!({ "error": "no such session" })),
        },
        "close" => match id {
            Some(id) => reply(&mut stream, json!({ "ok": daemon.close(id) })),
            None => reply(&mut stream, json!({ "error": "close needs an id" })),
        },
        other => reply(
            &mut stream,
            json!({ "error": format!("unknown op {other}") }),
        ),
    }
}

fn reply(stream: &mut UnixStream, value: Value) -> Result<()> {
    let mut body = serde_json::to_vec(&value)?;
    body.push(b'\n');
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
}

/// One full screen as a Lua table literal. Same shape `/snapshot.lua` serves:
/// `cols`, `rows`, `title`, `cursor`, `scroll` and `lines` of style runs.
fn lua(snapshot: &Snapshot) -> String {
    let mut out = String::from("return {\n");
    out.push_str(&format!(
        "  cols = {}, rows = {},\n  title = {},\n",
        snapshot.cols,
        snapshot.rows,
        lua_string(&snapshot.title)
    ));
    if let Some((x, y)) = snapshot.cursor {
        out.push_str(&format!(
            "  cursor = {{ x = {x}, y = {y}, visible = true }},\n"
        ));
    }
    if let Some(scroll) = snapshot.scroll {
        out.push_str(&format!(
            "  scroll = {{ offset = {}, total = {}, len = {} }},\n",
            scroll.offset, scroll.total, scroll.len
        ));
    }
    out.push_str("  lines = {\n");
    for y in 0..snapshot.rows {
        let mut runs: Vec<String> = Vec::new();
        let mut current: Option<(String, Option<String>, Option<String>, u8)> = None;
        let flush = |run: Option<(String, Option<String>, Option<String>, u8)>,
                     runs: &mut Vec<String>| {
            if let Some((text, fg, bg, flags)) = run {
                runs.push(format!(
                    "{{ {}, {}, {}, {} }}",
                    lua_string(&text),
                    fg.map_or("nil".to_string(), |value| lua_string(&value)),
                    bg.map_or("nil".to_string(), |value| lua_string(&value)),
                    flags
                ));
            }
        };
        for x in 0..snapshot.cols {
            let Some(cell) = snapshot.cell(x, y) else {
                break;
            };
            if cell.wide_tail {
                continue;
            }
            let text = if cell.text.is_empty() {
                " ".to_string()
            } else {
                cell.text.clone()
            };
            let fg = cell.fg.map(hex);
            let bg = cell.bg.map(hex);
            let mut flags = 0u8;
            if cell.bold {
                flags |= 1;
            }
            if cell.italic {
                flags |= 2;
            }
            if cell.underline {
                flags |= 4;
            }
            if cell.faint {
                flags |= 8;
            }
            if cell.inverse {
                flags |= 16;
            }
            match &mut current {
                Some((run_text, run_fg, run_bg, run_flags))
                    if *run_fg == fg && *run_bg == bg && *run_flags == flags =>
                {
                    run_text.push_str(&text);
                }
                _ => {
                    flush(current.take(), &mut runs);
                    current = Some((text, fg, bg, flags));
                }
            }
        }
        flush(current.take(), &mut runs);
        out.push_str(&format!(
            "    {{ y = {y}, runs = {{ {} }} }},\n",
            runs.join(", ")
        ));
    }
    out.push_str("  },\n}\n");
    out
}

fn hex(color: soil_vt::Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

fn lua_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A program on a pty, wired to the emulator.
pub struct Pty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    exited: Arc<AtomicBool>,
}

impl Pty {
    fn spawn(command: &[String], cols: u16, rows: u16, host: Host) -> Result<Self> {
        let first = command.first().context("empty command")?;
        let pair = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut builder = CommandBuilder::new(first);
        builder.args(&command[1..]);
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        builder.env("TERM_PROGRAM", "soil-termd");
        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
        let exited = Arc::new(AtomicBool::new(false));
        {
            let host = host.clone();
            let exited = Arc::clone(&exited);
            thread::Builder::new()
                .name("soil-termd-pty".into())
                .spawn(move || {
                    let mut buffer = [0u8; 8192];
                    loop {
                        match reader.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => host.write(&buffer[..n]),
                        }
                    }
                    exited.store(true, Ordering::Release);
                })?;
        }
        Ok(Self {
            master: Mutex::new(pair.master),
            writer,
            child: Mutex::new(child),
            exited,
        })
    }

    fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    fn exited(&self) -> bool {
        if let Ok(mut child) = self.child.lock() {
            if let Ok(Some(_)) = child.try_wait() {
                self.exited.store(true, Ordering::Release);
            }
        }
        self.exited.load(Ordering::Acquire)
    }
}

#[allow(dead_code)]
fn exists(path: &Path) -> bool {
    path.exists()
}
