//! Persistent host for the SFX Spore's Canvas surface.
//!
//! The guest is instantiated once and its `act` export is called for each pad
//! press. `soil:audio` therefore opens the room's output on the first press and
//! keeps it open, which is what makes the second press immediate. The same
//! process serves the browser host assets and a tiny action endpoint, so the
//! page does not need a second static server or a Python sheet.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use soil_audio_host::StaticRooms;
use soil_command::{Capabilities, CommandEntry, CommandInstance, CommandRunner};

const BRIDGE: &str = r#"
(() => {
  const original = window.fetch.bind(window);
  window.fetch = (input, init) => {
    let url = typeof input === "string" ? input : input.url;
    try {
      const parsed = new URL(url, location.href);
      if (parsed.pathname.endsWith("/v1/activate")) {
        const target = parsed.searchParams.get("target") || "";
        if (target.startsWith("sfx-pad-")) {
          const pad = target.slice("sfx-pad-".length);
          original("/act", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ action: "sfx.trigger", pad }),
          }).catch(() => {});
        }
      }
    } catch (_) {}
    return original(input, init);
  };
})();
"#;

fn main() {
    if let Err(error) = run() {
        eprintln!("sfx-host: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut module = default_root("SFX_MODULE", "src/fungOS/spores/spores/sfx/module.wasm");
    let mut vfs_root = default_root("FUNGOS_VFS_ROOT", ".local/share/fungos/vfs");
    let mut appdata_root = default_root("FUNGOS_APPDATA_ROOT", ".local/share/fungos/appdata");
    let mut web_root = default_root("SFX_WEB_ROOT", "src/fungOS/canvas-layout/crates/canvas-web/dist");
    let mut listen: SocketAddr = "127.0.0.1:8796".parse()?;
    let mut surface = "spore-dev-sfx".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--module" => module = PathBuf::from(args.next().context("--module PATH")?),
            "--vfs-root" => vfs_root = PathBuf::from(args.next().context("--vfs-root DIR")?),
            "--appdata-root" => {
                appdata_root = PathBuf::from(args.next().context("--appdata-root DIR")?)
            }
            "--web-root" => web_root = PathBuf::from(args.next().context("--web-root DIR")?),
            "--listen" => listen = args.next().context("--listen HOST:PORT")?.parse()?,
            "--surface" => surface = args.next().context("--surface NAME")?,
            "-h" | "--help" => {
                println!(
                    "usage: sfx-host [--module PATH] [--vfs-root DIR] [--appdata-root DIR]\\n                 [--web-root DIR] [--listen HOST:PORT] [--surface NAME]"
                );
                return Ok(());
            }
            other => bail!("unknown option {other}"),
        }
    }
    if !listen.ip().is_loopback() {
        bail!("sfx-host serves a private action endpoint; listen must be loopback");
    }

    if !module.is_file() {
        bail!("SFX module not found at {}", module.display());
    }
    let entry = CommandEntry {
        name: "sfx".into(),
        version: "0.1.0".into(),
        summary: "Event-driven soundboard".into(),
        module,
        timeout_ms: 120_000,
        max_stdout_bytes: 1024 * 1024,
        max_stderr_bytes: 1024 * 1024,
        max_memory_bytes: 256 * 1024 * 1024,
    };
    let runner = CommandRunner::with_capabilities(
        None,
        Capabilities {
            vfs_root: Some(vfs_root),
            appdata_root: Some(appdata_root),
            app_id: Some("dev.fungos.sfx".into()),
            user_id: None,
            rooms: Some(Arc::new(StaticRooms::from_env())),
        },
    )?;
    let mut instance = runner.instantiate(&entry)?;
    let listener = TcpListener::bind(listen)?;
    eprintln!("sfx-host: {listen} · surface={surface} · web={}", web_root.display());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) = handle(stream, &mut instance, &web_root, &surface) {
                    eprintln!("sfx-host request: {error:#}");
                }
            }
            Err(error) => eprintln!("sfx-host accept: {error}"),
        }
    }
    Ok(())
}

fn default_root(env: &str, fallback: &str) -> PathBuf {
    std::env::var_os(env)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(fallback)
        })
}

fn handle(
    mut stream: TcpStream,
    instance: &mut CommandInstance,
    web_root: &Path,
    surface: &str,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let request = read_request(&mut stream)?;
    if request.method == "OPTIONS" {
        return respond(&mut stream, 204, "text/plain", b"");
    }
    if request.method == "POST" && request.path == "/act" {
        let body = String::from_utf8(request.body).context("action body must be UTF-8")?;
        let action: serde_json::Value = serde_json::from_str(&body).context("action body must be JSON")?;
        if action.get("action").and_then(|value| value.as_str()) != Some("sfx.trigger") {
            bail!("only sfx.trigger is accepted from the browser");
        }
        let snapshot = instance.act(&body)?;
        return respond(&mut stream, 200, "application/json", &snapshot);
    }
    if request.method == "GET" && (request.path == "/snapshot" || request.path == "/api/snapshot") {
        let snapshot = instance.snapshot()?;
        return respond(&mut stream, 200, "application/json", &snapshot);
    }
    if request.method == "GET" && request.path == "/health" {
        return respond(&mut stream, 200, "text/plain", b"ok");
    }
    if request.method == "GET" && request.path == "/sfx-bridge.js" {
        return respond(&mut stream, 200, "text/javascript", BRIDGE.as_bytes());
    }
    if request.method != "GET" {
        return respond(&mut stream, 405, "text/plain", b"method not allowed");
    }

    let relative = request.path.trim_start_matches('/');
    if relative.split('/').any(|part| part == "..") {
        return respond(&mut stream, 400, "text/plain", b"invalid path");
    }
    let mut path = web_root.join(relative);
    if path.is_dir() {
        path = path.join("index.html");
    }
    if !path.is_file() {
        return respond(&mut stream, 404, "text/plain", b"not found");
    }
    let body = std::fs::read(&path)?;
    if path.file_name().and_then(|name| name.to_str()) == Some("index.html") {
        let body = inject_index(&body, surface);
        return respond(&mut stream, 200, "text/html", &body);
    }
    let content_type = match path.extension().and_then(|extension| extension.to_str()) {
        Some("js") => "text/javascript",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    };
    respond(&mut stream, 200, content_type, &body)
}

fn inject_index(body: &[u8], surface: &str) -> Vec<u8> {
    let source = String::from_utf8_lossy(body);
    let redirect = format!(
        "<script>if(!new URLSearchParams(location.search).has('surface')){{location.replace('/?surface='+encodeURIComponent({surface:?}));}}</script>"
    );
    let bridge = r#"<script type="module" src="/sfx-bridge.js"></script>"#;
    let source = source.replacen("<head>", &format!("<head>\n{redirect}"), 1);
    source.replacen("</body>", &format!("{bridge}\n</body>"), 1).into_bytes()
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end;
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("connection closed before headers");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_header_end(&buffer) {
            header_end = position;
            break;
        }
        if buffer.len() > 64 * 1024 {
            bail!("request headers exceed 64 KiB");
        }
    }
    let header = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header.split("\r\n");
    let request_line = lines.next().context("request has no request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().context("request has no method")?.to_owned();
    let path = parts.next().context("request has no path")?.split('?').next().unwrap_or("/").to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    if length > 64 * 1024 {
        bail!("request body exceeds 64 KiB");
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Ok(Request { method, path, body })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: content-type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}
