//! A browser terminal with a target picker, rendered by `soil-vt`.
//!
//! ```text
//! browser ── SSE grid ── soil-terminal-web ── pty ── zsh | ssh | mush
//!                 └──── POST input ────┘
//! ```
//!
//! The emulator is the same `soil-vt` the native terminal window uses. The
//! browser is a second presenter, not a second terminal: it receives styled
//! rows and sends keystrokes.
//!
//! One server hosts many sessions. A session is a target (a local shell, an
//! ssh host, or a Mush desktop) on its own pty and its own emulator; the page
//! opens, switches, and closes them like a multiplexer.
//!
//! ```sh
//! # pick from ~/.config/fungos/terminal/targets.toml, or from what is on the
//! # machine: the login shells, the Host aliases in ~/.ssh/config, and the
//! # desktops under ~/.config/fungos/desktops/
//! soil-terminal-web
//! # one target, no picker
//! soil-terminal-web --ssh aj@home-pi
//! soil-terminal-web --command target/debug/mush \
//!   --wasm spores/mush/module.wasm --root ~/.local/share/fungos/vfs
//! ```
//!
//! The listener defaults to loopback. A non-loopback listener requires a token,
//! because this is a remote shell: put it behind an SSH tunnel or a trusted
//! reverse proxy, and do not expose it to the open internet.

mod frame;
mod http;
mod page;
mod paste;
mod pty;
mod targets;

use std::collections::BTreeMap;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use soil_vt::{Host, Snapshot};

use http::{query_param, read_request, respond, respond_json};
use paste::Paste;
use pty::Pty;
use targets::{Kind, Target};

const DEFAULT_LISTEN: &str = "127.0.0.1:8790";
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_SCROLLBACK: usize = 5_000;
const MAX_DIMENSION: u16 = 500;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const KEEPALIVE: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SCROLL: i64 = 10_000;

const USAGE: &str = "\
soil-terminal-web [OPTIONS]

  --listen ADDR        bind address (default 127.0.0.1:8790)
  --token VALUE        require ?token=VALUE (mandatory beyond loopback)
  --targets FILE       target catalog (default ~/.config/fungos/terminal/targets.toml)
  --ssh TARGET         run `ssh -tt TARGET` as the only target
  --ssh-arg VALUE      extra ssh argument, repeatable (e.g. -p 2222)
  --command CMD [ARGS] run CMD as the only target; consumes the rest
  --paste-root DIR     where pasted bytes are stored (default ~/.local/share/fungos/paste)
  --cols N             initial columns (default 80)
  --rows N             initial rows (default 24)
  --scrollback N       scrollback lines (default 5000)
  -h, --help           show this help";

struct Args {
    listen: String,
    token: Option<String>,
    command: Vec<String>,
    ssh: Option<String>,
    ssh_args: Vec<String>,
    cols: u16,
    rows: u16,
    scrollback: usize,
    paste_root: PathBuf,
    targets: Option<PathBuf>,
}

fn main() -> Result<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let targets = resolve_targets(&args)?;
    let app = Arc::new(App {
        token: args.token,
        paste: Paste::open(args.paste_root.clone())?,
        targets,
        sessions: Mutex::new(BTreeMap::new()),
        next_id: AtomicU64::new(1),
        cols: args.cols,
        rows: args.rows,
        scrollback: args.scrollback,
    });

    let listener =
        TcpListener::bind(&args.listen).with_context(|| format!("bind {}", args.listen))?;
    match &app.token {
        Some(token) => println!("soil terminal: http://{}/?token={token}", args.listen),
        None => println!("soil terminal: http://{}/", args.listen),
    }
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                thread::spawn(move || {
                    if let Err(error) = handle(stream, &app) {
                        eprintln!("soil terminal: request failed: {error:#}");
                    }
                });
            }
            Err(error) => eprintln!("soil terminal: accept failed: {error}"),
        }
    }
    Ok(())
}

struct App {
    token: Option<String>,
    paste: Paste,
    targets: Vec<Target>,
    sessions: Mutex<BTreeMap<u64, Arc<Session>>>,
    next_id: AtomicU64,
    cols: u16,
    rows: u16,
    scrollback: usize,
}

struct Session {
    id: u64,
    target: String,
    label: String,
    host: Host,
    pty: Pty,
}

impl App {
    fn open(&self, target: &Target) -> Result<Arc<Session>> {
        // A target has at most one live session: opening it again attaches. Mush
        // takes a lock per desktop and a second instance exits 0 with no output,
        // so a duplicate is not just wasteful, it looks like a crash.
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(existing) = sessions
                .values()
                .find(|session| session.target == target.name && !session.pty.exited())
            {
                return Ok(Arc::clone(existing));
            }
            let stale: Vec<u64> = sessions
                .values()
                .filter(|session| session.target == target.name)
                .map(|session| session.id)
                .collect();
            for id in stale {
                if let Some(session) = sessions.remove(&id) {
                    session.pty.kill();
                }
            }
        }
        if let Some(program) = target.command.first()
            && !targets::program_exists(program)
        {
            anyhow::bail!("{program} is not installed (target {:?})", target.name);
        }
        let host = Host::spawn(self.cols, self.rows, self.scrollback)
            .map_err(|error| anyhow::anyhow!("terminal: {error}"))?;
        let pty = Pty::spawn(&target.command, self.cols, self.rows, host.clone())?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let session = Arc::new(Session {
            id,
            target: target.name.clone(),
            label: target.label.clone(),
            host,
            pty,
        });
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(id, Arc::clone(&session));
        }
        Ok(session)
    }

    fn session(&self, id: u64) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&id).cloned())
    }

    /// The session an endpoint means: the one named in `?session=`, or the
    /// first one for a caller that predates the picker.
    fn pick(&self, id: Option<u64>) -> Option<Arc<Session>> {
        match id {
            Some(id) => self.session(id),
            None => self
                .sessions
                .lock()
                .ok()
                .and_then(|sessions| sessions.values().next().cloned()),
        }
    }
}

fn handle(mut stream: TcpStream, app: &App) -> Result<()> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let Some(request) = read_request(&mut stream)? else {
        return Ok(());
    };
    if request.too_large {
        respond(
            &mut stream,
            "413 Content Too Large",
            "text/plain; charset=utf-8",
            b"request body too large\n",
        )?;
        return Ok(());
    }
    if let Some(expected) = &app.token
        && query_param(&request.target, "token").as_deref() != Some(expected)
    {
        respond(
            &mut stream,
            "401 Unauthorized",
            "text/plain; charset=utf-8",
            b"token required\n",
        )?;
        return Ok(());
    }
    let path = request
        .target
        .split_once('?')
        .map_or(request.target.as_str(), |(path, _)| path);
    let wanted =
        query_param(&request.target, "session").and_then(|value| value.parse::<u64>().ok());
    match (request.method.as_str(), path) {
        ("GET", "/") | ("GET", "/index.html") => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            page::INDEX_HTML.as_bytes(),
        )?,
        ("GET", "/health") => respond(&mut stream, "200 OK", "text/plain; charset=utf-8", b"ok\n")?,
        ("GET", "/favicon.ico") => respond(&mut stream, "204 No Content", "text/plain", b"")?,
        ("GET", "/targets") => respond_json(&mut stream, "200 OK", &targets_json(app))?,
        ("GET", "/sessions") => respond_json(&mut stream, "200 OK", &sessions_json(app))?,
        ("POST", "/sessions") => open_session(&mut stream, app, &request.body)?,
        ("DELETE", path) if path.starts_with("/sessions/") => {
            close_session(&mut stream, app, path)?
        }
        ("GET", "/events") => {
            let Some(session) = app.pick(wanted) else {
                return not_found(&mut stream);
            };
            return sse(stream, session);
        }
        ("POST", "/input") => {
            let Some(session) = app.pick(wanted) else {
                return not_found(&mut stream);
            };
            // Typing while reading history means "back to the prompt".
            session.host.scroll_to_bottom();
            session.pty.write(&request.body);
            respond(&mut stream, "204 No Content", "text/plain", b"")?;
        }
        ("POST", "/scroll") => {
            let Some(session) = app.pick(wanted) else {
                return not_found(&mut stream);
            };
            let Some(delta) = parse_scroll(&request.body) else {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain; charset=utf-8",
                    b"expected {\"delta\":N}\n",
                )?;
                return Ok(());
            };
            session.host.scroll(delta);
            respond(&mut stream, "204 No Content", "text/plain", b"")?;
        }
        ("POST", "/paste") => {
            let name = request
                .header("x-filename")
                .map(http::percent_decode)
                .unwrap_or_default();
            let media_type = request
                .header("content-type")
                .unwrap_or("application/octet-stream")
                .to_owned();
            match app.paste.store(&request.body, &name, &media_type) {
                Ok(stored) => respond_json(&mut stream, "200 OK", &stored.to_json())?,
                Err(error) => {
                    eprintln!("soil terminal: paste failed: {error:#}");
                    respond(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain; charset=utf-8",
                        b"paste failed\n",
                    )?;
                }
            }
        }
        ("POST", "/resize") => {
            let Some(session) = app.pick(wanted) else {
                return not_found(&mut stream);
            };
            let Some((cols, rows)) = parse_size(&request.body) else {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain; charset=utf-8",
                    b"expected {\"cols\":N,\"rows\":N}\n",
                )?;
                return Ok(());
            };
            session.host.resize(cols, rows);
            session.pty.resize(cols, rows);
            respond(&mut stream, "204 No Content", "text/plain", b"")?;
        }
        ("GET" | "POST" | "DELETE", _) => return not_found(&mut stream),
        _ => respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"method not allowed\n",
        )?,
    }
    Ok(())
}

fn not_found(stream: &mut TcpStream) -> Result<()> {
    respond(
        stream,
        "404 Not Found",
        "text/plain; charset=utf-8",
        b"no such session or path\n",
    )?;
    Ok(())
}

fn targets_json(app: &App) -> Value {
    json!({
        "targets": app
            .targets
            .iter()
            .map(|target| json!({
                "name": target.name,
                "label": target.label,
                "kind": target.kind.as_str(),
                "detail": target.detail,
            }))
            .collect::<Vec<_>>()
    })
}

fn sessions_json(app: &App) -> Value {
    let sessions = app
        .sessions
        .lock()
        .map(|sessions| {
            sessions
                .values()
                .map(|session| {
                    json!({
                        "id": session.id,
                        "target": session.target,
                        "label": session.label,
                        "exited": session.pty.exited(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({ "sessions": sessions })
}

fn open_session(stream: &mut TcpStream, app: &App, body: &[u8]) -> Result<()> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        respond(
            stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            b"expected {\"target\":\"name\"}\n",
        )?;
        return Ok(());
    };
    let Some(name) = value.get("target").and_then(Value::as_str) else {
        respond(
            stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            b"expected {\"target\":\"name\"}\n",
        )?;
        return Ok(());
    };
    let Some(target) = app.targets.iter().find(|target| target.name == name) else {
        respond(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            format!("no target {name:?}\n").as_bytes(),
        )?;
        return Ok(());
    };
    match app.open(target) {
        Ok(session) => respond_json(
            stream,
            "200 OK",
            &json!({ "id": session.id, "target": session.target, "label": session.label }),
        )?,
        Err(error) => {
            eprintln!("soil terminal: could not start {}: {error:#}", target.name);
            respond(
                stream,
                "500 Internal Server Error",
                "text/plain; charset=utf-8",
                format!("could not start {}: {error:#}\n", target.name).as_bytes(),
            )?;
        }
    }
    Ok(())
}

fn close_session(stream: &mut TcpStream, app: &App, path: &str) -> Result<()> {
    let Ok(id) = path.trim_start_matches("/sessions/").parse::<u64>() else {
        return not_found(stream);
    };
    let removed = app
        .sessions
        .lock()
        .ok()
        .and_then(|mut sessions| sessions.remove(&id));
    match removed {
        Some(session) => {
            session.pty.kill();
            respond(stream, "204 No Content", "text/plain", b"")?;
        }
        None => not_found(stream)?,
    }
    Ok(())
}

fn parse_scroll(body: &[u8]) -> Option<isize> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let delta = value.get("delta")?.as_i64()?;
    Some(delta.clamp(-MAX_SCROLL, MAX_SCROLL) as isize)
}

fn parse_size(body: &[u8]) -> Option<(u16, u16)> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let cols = value.get("cols")?.as_u64()?;
    let rows = value.get("rows")?.as_u64()?;
    Some((
        cols.clamp(1, u64::from(MAX_DIMENSION)) as u16,
        rows.clamp(1, u64::from(MAX_DIMENSION)) as u16,
    ))
}

fn sse(mut stream: TcpStream, session: Arc<Session>) -> Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\n",
    )?;
    stream.flush()?;

    let mut last: Option<Arc<Snapshot>> = None;
    let mut last_generation = u64::MAX;
    let mut exit_sent = false;
    let mut keepalive = Instant::now();
    loop {
        let generation = session.host.generation();
        if last.is_none() || generation != last_generation {
            let snapshot = session.host.snapshot();
            let frame = frame::frame(&snapshot, last.as_deref());
            write_event(&mut stream, &frame)?;
            last = Some(snapshot);
            last_generation = generation;
        }
        if session.pty.exited() && !exit_sent {
            write_event(&mut stream, &json!({ "type": "exit" }))?;
            exit_sent = true;
        }
        if keepalive.elapsed() >= KEEPALIVE {
            stream.write_all(b": keepalive\n\n")?;
            stream.flush()?;
            keepalive = Instant::now();
        }
        thread::sleep(FRAME_INTERVAL);
    }
}

fn write_event(stream: &mut TcpStream, value: &Value) -> Result<()> {
    let body = serde_json::to_string(value)?;
    stream.write_all(format!("data: {body}\n\n").as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn parse_args() -> Result<Option<Args>> {
    let mut args = Args {
        listen: DEFAULT_LISTEN.into(),
        token: None,
        command: Vec::new(),
        ssh: None,
        ssh_args: Vec::new(),
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
        scrollback: DEFAULT_SCROLLBACK,
        paste_root: expand_home("~/.local/share/fungos/paste"),
        targets: None,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--listen" => args.listen = iter.next().context("--listen needs ADDR")?,
            "--token" => args.token = Some(iter.next().context("--token needs a value")?),
            "--ssh" => args.ssh = Some(iter.next().context("--ssh needs TARGET")?),
            "--ssh-arg" => args
                .ssh_args
                .push(iter.next().context("--ssh-arg needs a value")?),
            "--targets" => {
                args.targets = Some(expand_home(&iter.next().context("--targets needs FILE")?))
            }
            "--cols" => {
                args.cols = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .context("--cols needs a number")?
            }
            "--rows" => {
                args.rows = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .context("--rows needs a number")?
            }
            "--scrollback" => {
                args.scrollback = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .context("--scrollback needs a number")?
            }
            "--command" => {
                args.command = iter.collect();
                break;
            }
            "--paste-root" => {
                args.paste_root = expand_home(&iter.next().context("--paste-root needs DIR")?)
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            other => {
                if let Some(value) = other.strip_prefix("--ssh-arg=") {
                    args.ssh_args.push(value.to_owned());
                } else if let Some(value) = other.strip_prefix("--paste-root=") {
                    args.paste_root = expand_home(value);
                } else if let Some(value) = other.strip_prefix("--targets=") {
                    args.targets = Some(expand_home(value));
                } else {
                    anyhow::bail!("unknown argument {other}; try --help");
                }
            }
        }
    }

    if args.cols == 0 || args.cols > MAX_DIMENSION || args.rows == 0 || args.rows > MAX_DIMENSION {
        anyhow::bail!("cols and rows must be 1-{MAX_DIMENSION}");
    }
    if args.scrollback == 0 {
        anyhow::bail!("--scrollback must be positive");
    }
    if !is_loopback(&args.listen) && args.token.is_none() {
        anyhow::bail!(
            "--token is required when listening beyond loopback ({})",
            args.listen
        );
    }
    Ok(Some(args))
}

fn resolve_targets(args: &Args) -> Result<Vec<Target>> {
    if args.command.is_empty() && args.ssh.is_none() {
        let targets = targets::load(args.targets.as_deref())?;
        if targets.is_empty() {
            anyhow::bail!(
                "no targets: add [[target]] entries to ~/.config/fungos/terminal/targets.toml, or pass --ssh/--command"
            );
        }
        return Ok(targets);
    }
    if args.targets.is_some() {
        anyhow::bail!("--targets cannot be combined with --ssh/--command");
    }
    let command = resolve_command(args.command.clone(), args.ssh.as_deref(), &args.ssh_args)?;
    let label = command
        .first()
        .cloned()
        .unwrap_or_else(|| "shell".to_owned());
    Ok(vec![Target {
        name: "default".into(),
        label,
        kind: if args.ssh.is_some() {
            Kind::Ssh
        } else {
            Kind::Shell
        },
        detail: command.join(" "),
        command,
    }])
}

fn expand_home(value: &str) -> PathBuf {
    targets::expand_home(value)
}

fn resolve_command(
    command: Vec<String>,
    ssh: Option<&str>,
    ssh_args: &[String],
) -> Result<Vec<String>> {
    if !command.is_empty() {
        if ssh.is_some() {
            anyhow::bail!("--command and --ssh are mutually exclusive");
        }
        return Ok(command);
    }
    Ok(match ssh {
        Some(target) => {
            let mut command = vec!["ssh".to_string(), "-tt".to_string()];
            command.extend(ssh_args.iter().cloned());
            command.push(target.to_owned());
            command
        }
        None => vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())],
    })
}

fn is_loopback(listen: &str) -> bool {
    let host = listen.rsplit_once(':').map_or(listen, |(host, _)| host);
    host == "localhost" || host == "::1" || host == "[::1]" || host.starts_with("127.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_is_loopback() {
        assert!(is_loopback("127.0.0.1:8790"));
        assert!(is_loopback("localhost:8790"));
        assert!(is_loopback("[::1]:8790"));
        assert!(!is_loopback("0.0.0.0:8790"));
        assert!(!is_loopback("192.168.1.5:8790"));
    }

    #[test]
    fn sizes_are_clamped_and_missing_fields_are_rejected() {
        assert_eq!(parse_size(br#"{"cols":100,"rows":40}"#), Some((100, 40)));
        assert_eq!(parse_size(br#"{"cols":0,"rows":9000}"#), Some((1, 500)));
        assert_eq!(parse_size(br#"{"cols":100}"#), None);
        assert_eq!(parse_size(b"not json"), None);
    }

    #[test]
    fn scroll_deltas_are_clamped() {
        assert_eq!(parse_scroll(br#"{"delta":-5}"#), Some(-5));
        assert_eq!(parse_scroll(br#"{"delta":999999}"#), Some(10_000));
        assert_eq!(parse_scroll(b"{}"), None);
    }

    #[test]
    fn ssh_builds_the_expected_argv() {
        let command = resolve_command(
            Vec::new(),
            Some("aj@home-pi"),
            &["-p".into(), "2222".into()],
        )
        .unwrap();
        assert_eq!(command, ["ssh", "-tt", "-p", "2222", "aj@home-pi"]);
    }

    #[test]
    fn command_and_ssh_cannot_be_combined() {
        let error = resolve_command(vec!["sh".into()], Some("pi"), &[]).unwrap_err();
        assert!(error.to_string().contains("mutually exclusive"), "{error}");
    }

    #[test]
    fn a_live_session_is_reused_for_the_same_target() {
        let dir = tempfile::tempdir().unwrap();
        let app = App {
            token: None,
            paste: Paste::open(dir.path().to_path_buf()).unwrap(),
            targets: Vec::new(),
            sessions: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            cols: 40,
            rows: 6,
            scrollback: 100,
        };
        let target = Target {
            name: "t".into(),
            label: "t".into(),
            kind: Kind::Shell,
            detail: String::new(),
            command: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
        };
        let first = app.open(&target).unwrap();
        let second = app.open(&target).unwrap();
        assert_eq!(first.id, second.id);
    }
}
