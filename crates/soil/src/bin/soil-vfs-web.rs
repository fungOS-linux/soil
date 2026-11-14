//! Browser inspector for a `fungos-vfs` workspace served through `soil:vfs`
//! and `soil:index`.
//!
//! ```sh
//! cargo run -p soil-host --bin soil-vfs-web -- \
//!   --root ~/.local/share/fungos/vfs --listen 127.0.0.1:8788
//! ```
//!
//! This is a read-only local tool: it lists the namespace, searches the derived
//! index, and previews bounded file ranges. It never exposes a host path to the
//! browser.

use std::{
    env,
    io::{self, BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use soil::vfs_host::{VfsHost, index, vfs};

const DEFAULT_ROOT: &str = "~/.local/share/fungos/vfs";
const DEFAULT_LISTEN: &str = "127.0.0.1:8788";
const PAGE: u32 = 512;
const MAX_ENTRIES: usize = 10_000;
const CHUNK: u32 = 1024 * 1024;
const MAX_PREVIEW: usize = 16 * 1024 * 1024;
const MAX_DOWNLOAD: usize = 64 * 1024 * 1024;

struct App {
    host: Mutex<VfsHost>,
    root: String,
}

struct Args {
    root: PathBuf,
    listen: String,
}

fn main() -> Result<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    std::fs::create_dir_all(&args.root)
        .with_context(|| format!("create {}", args.root.display()))?;
    let host =
        VfsHost::open(&args.root).with_context(|| format!("open {}", args.root.display()))?;
    let app = Arc::new(App {
        host: Mutex::new(host),
        root: args.root.display().to_string(),
    });
    let listener =
        TcpListener::bind(&args.listen).with_context(|| format!("bind {}", args.listen))?;
    println!(
        "soil vfs: http://{}/  root={}",
        args.listen,
        args.root.display()
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                thread::spawn(move || {
                    if let Err(error) = handle(stream, &app) {
                        eprintln!("soil vfs: request failed: {error:#}");
                    }
                });
            }
            Err(error) => eprintln!("soil vfs: accept failed: {error}"),
        }
    }
    Ok(())
}

fn handle(mut stream: TcpStream, app: &App) -> Result<()> {
    let Some((method, target)) = read_request(&mut stream)? else {
        return Ok(());
    };
    if method != "GET" && method != "HEAD" {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed\n",
        );
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target.as_str(), ""),
    };

    match path {
        "/" | "/index.html" => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        "/health" => respond(&mut stream, "200 OK", "text/plain; charset=utf-8", b"ok\n"),
        "/api/entries" => {
            let parent = query_param(query, "path").unwrap_or_default();
            let entries = list_entries(app, &parent)?;
            let state = index_state(app)?;
            respond_json(
                &mut stream,
                "200 OK",
                &json!({
                    "root": app.root,
                    "path": parent,
                    "entries": entries.iter().map(entry_json).collect::<Vec<_>>(),
                    "index": state,
                }),
            )
        }
        "/api/search" => {
            let text = query_param(query, "q").unwrap_or_default();
            let mode = query_param(query, "mode").unwrap_or_else(|| "substring".into());
            let hits = search(app, &text, &mode)?;
            respond_json(
                &mut stream,
                "200 OK",
                &json!({
                    "root": app.root,
                    "query": text,
                    "mode": mode,
                    "hits": hits,
                }),
            )
        }
        "/api/stat" => {
            let path = query_param(query, "path").unwrap_or_default();
            let entry = stat(app, &path)?;
            respond_json(&mut stream, "200 OK", &entry_json(&entry))
        }
        "/api/content" => {
            let path = query_param(query, "path").unwrap_or_default();
            let download = query_param(query, "download").as_deref() == Some("1");
            let entry = stat(app, &path)?;
            if matches!(entry.kind, vfs::Kind::Directory) {
                return respond(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    b"is a directory\n",
                );
            }
            let max = if download { MAX_DOWNLOAD } else { MAX_PREVIEW };
            let bytes = read_file(app, &path, max)?;
            if download {
                let filename = entry.name.replace(['"', '\\', '\r', '\n'], "_");
                return respond_attachment(&mut stream, &entry.media_type, &filename, &bytes);
            }
            respond(
                &mut stream,
                "200 OK",
                if entry.media_type.is_empty() {
                    "application/octet-stream"
                } else {
                    &entry.media_type
                },
                &bytes,
            )
        }
        "/favicon.ico" => respond(&mut stream, "204 No Content", "text/plain", b""),
        _ => respond(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found\n",
        ),
    }
}

fn list_entries(app: &App, parent: &str) -> Result<Vec<vfs::Entry>> {
    let mut host = app.host.lock().expect("vfs host poisoned");
    let mut offset = 0u64;
    let mut entries = Vec::new();
    loop {
        let page = <VfsHost as vfs::Host>::ls(&mut *host, parent.to_string(), offset, PAGE)
            .map_err(vfs_error)?;
        let count = page.entries.len();
        entries.extend(page.entries);
        if entries.len() >= MAX_ENTRIES {
            entries.truncate(MAX_ENTRIES);
            break;
        }
        match page.next {
            Some(next) if count > 0 && next > offset => offset = next,
            _ => break,
        }
    }
    entries.sort_by(|a, b| {
        let a_dir = matches!(a.kind, vfs::Kind::Directory);
        let b_dir = matches!(b.kind, vfs::Kind::Directory);
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

fn stat(app: &App, path: &str) -> Result<vfs::Entry> {
    let mut host = app.host.lock().expect("vfs host poisoned");
    <VfsHost as vfs::Host>::stat(&mut *host, path.to_string()).map_err(vfs_error)
}

fn search(app: &App, text: &str, mode: &str) -> Result<Vec<Value>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mode = match mode {
        "fuzzy" => Some(index::Mode::Fuzzy),
        "full-text" => Some(index::Mode::FullText),
        "semantic" => Some(index::Mode::Semantic),
        _ => Some(index::Mode::Substring),
    };
    let mut host = app.host.lock().expect("vfs host poisoned");
    let page = <VfsHost as index::Host>::search(
        &mut *host,
        index::Query {
            text: text.to_string(),
            mode,
            parent: None,
            media_type_prefix: None,
            offset: 0,
            limit: PAGE,
        },
    )
    .map_err(index_error)?;
    let mut hits = Vec::new();
    for hit in page.hits {
        let entry =
            <VfsHost as vfs::Host>::stat(&mut *host, hit.path.clone()).map_err(vfs_error)?;
        hits.push(json!({
            "entry": entry_json(&entry),
            "snippet": hit.snippet,
            "score": hit.score,
            "matched_by": hit.matched_by.map(|source| format!("{source:?}").to_lowercase()),
        }));
    }
    Ok(hits)
}

fn index_state(app: &App) -> Result<Value> {
    let mut host = app.host.lock().expect("vfs host poisoned");
    let state = <VfsHost as index::Host>::state(&mut *host).map_err(index_error)?;
    Ok(json!({
        "ready": state.ready,
        "generation": state.generation,
        "entries": state.entries,
        "rebuilt_at_ms": state.rebuilt_at_ms,
    }))
}

fn read_file(app: &App, path: &str, max: usize) -> Result<Vec<u8>> {
    let mut host = app.host.lock().expect("vfs host poisoned");
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    loop {
        let chunk = <VfsHost as vfs::Host>::read(&mut *host, path.to_string(), offset, CHUNK)
            .map_err(vfs_error)?;
        let count = chunk.data.len();
        bytes.extend(chunk.data);
        if chunk.eof || count == 0 || bytes.len() >= max {
            break;
        }
        offset = offset.saturating_add(count as u64);
    }
    bytes.truncate(max);
    Ok(bytes)
}

fn entry_json(entry: &vfs::Entry) -> Value {
    json!({
        "path": entry.path,
        "name": entry.name,
        "kind": match entry.kind {
            vfs::Kind::File => "file",
            vfs::Kind::Directory => "directory",
        },
        "size": entry.size,
        "content_id": entry.content_id,
        "media_type": if entry.media_type.is_empty() { "application/octet-stream" } else { &entry.media_type },
        "version": entry.version,
        "hydration": match entry.hydration {
            vfs::Hydration::Present => "present",
            vfs::Hydration::Placeholder => "placeholder",
        },
        "pinned": entry.pinned,
        "updated_at_ms": entry.updated_at_ms,
        "child_count": entry.child_count,
    })
}

fn vfs_error(error: vfs::Error) -> anyhow::Error {
    anyhow::anyhow!("soil:vfs {error:?}")
}

fn index_error(error: index::Error) -> anyhow::Error {
    anyhow::anyhow!("soil:index {error:?}")
}

fn read_request(stream: &mut TcpStream) -> io::Result<Option<(String, String)>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    if reader.read_line(&mut request)? == 0 {
        return Ok(None);
    }
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
    }
    Ok(Some((method, target)))
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> Result<()> {
    stream.write_all(
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.write_all(body)?;
    Ok(())
}

fn respond_json(stream: &mut TcpStream, status: &str, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    respond(stream, status, "application/json; charset=utf-8", &body)
}

fn respond_attachment(
    stream: &mut TcpStream,
    content_type: &str,
    filename: &str,
    body: &[u8],
) -> Result<()> {
    stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nContent-Disposition: attachment; filename=\"{filename}\"\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.write_all(body)?;
    Ok(())
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if percent_decode(key) == name {
            Some(percent_decode(value))
        } else {
            None
        }
    })
}

fn percent_decode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte as char);
                    index += 3;
                    continue;
                }
                out.push('%');
                index += 1;
            }
            b'+' => {
                out.push(' ');
                index += 1;
            }
            byte => {
                out.push(byte as char);
                index += 1;
            }
        }
    }
    out
}

fn parse_args() -> Result<Option<Args>> {
    let mut args = Args {
        root: PathBuf::from(expand_home(DEFAULT_ROOT)),
        listen: DEFAULT_LISTEN.into(),
    };
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--root" => {
                args.root =
                    PathBuf::from(expand_home(&iter.next().context("--root needs a value")?));
            }
            "--listen" => args.listen = iter.next().context("--listen needs a value")?,
            "-h" | "--help" => {
                println!(
                    "soil-vfs-web [--root DIR] [--listen ADDR]\n\
                     defaults: root={DEFAULT_ROOT} listen={DEFAULT_LISTEN}"
                );
                return Ok(None);
            }
            other => anyhow::bail!("unknown argument {other}; try --help"),
        }
    }
    Ok(Some(args))
}

fn expand_home(value: &str) -> String {
    if value == "~" {
        return env::var("HOME").unwrap_or_else(|_| value.into());
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Ok(home) = env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    value.into()
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Soil VFS</title>
<style>
:root {
  color-scheme: dark;
  --bg: #0b0e11; --panel: #12171c; --panel-2: #171e25; --line: #26313b;
  --text: #dce5ea; --muted: #8395a2; --accent: #7dd3a8; --accent-2: #5bb8ff; --dir: #f0c674;
}
* { box-sizing: border-box; }
html, body { height: 100%; }
body {
  margin: 0; background: radial-gradient(circle at 20% -10%, #16232a 0, var(--bg) 45%);
  color: var(--text); font: 15px/1.45 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
header {
  display: flex; align-items: center; gap: 16px; padding: 14px 20px;
  border-bottom: 1px solid var(--line); background: rgba(11, 14, 17, 0.86);
  backdrop-filter: blur(12px); position: sticky; top: 0; z-index: 2;
}
.brand { display: flex; align-items: center; gap: 9px; font-weight: 650; letter-spacing: 0.02em; }
.dot { width: 10px; height: 10px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 14px rgba(125, 211, 168, 0.8); }
.status { color: var(--muted); font-size: 12px; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; max-width: 46vw; }
header .spacer { flex: 1; }
.button {
  color: var(--text); text-decoration: none; font-size: 13px; border: 1px solid var(--line);
  background: var(--panel-2); border-radius: 8px; padding: 6px 11px; cursor: pointer;
}
.button:hover { border-color: #3b4c5c; }
main { display: flex; height: calc(100vh - 59px); }
aside { flex: 0 0 clamp(180px, 30vw, 320px); border-right: 1px solid var(--line); min-height: 0; display: flex; flex-direction: column; }
.toolbar { display: flex; gap: 7px; align-items: center; padding: 12px 12px 7px; }
.toolbar .button { padding: 6px 9px; }
.crumb { display: flex; flex-wrap: wrap; gap: 3px; align-items: center; min-width: 0; font-size: 13px; }
.crumb button { border: 0; background: transparent; color: var(--accent-2); padding: 2px 4px; cursor: pointer; font: inherit; border-radius: 5px; }
.crumb button:hover { background: var(--panel); }
.crumb span { color: var(--muted); }
.searchbar { display: flex; gap: 7px; padding: 5px 12px 8px; }
input, select {
  border: 1px solid var(--line); background: var(--panel); color: var(--text);
  border-radius: 9px; padding: 9px 11px; font: inherit; outline: none;
}
input { flex: 1; min-width: 0; }
select { max-width: 104px; font-size: 12px; }
input:focus, select:focus { border-color: var(--accent-2); box-shadow: 0 0 0 3px rgba(91, 184, 255, 0.12); }
.count { padding: 0 16px 8px; color: var(--muted); font-size: 12px; }
.file-list { list-style: none; padding: 0 8px 20px; margin: 0; overflow: auto; min-height: 0; }
.file-row {
  display: flex; gap: 11px; align-items: center; padding: 9px 10px;
  border-radius: 9px; cursor: pointer; border: 1px solid transparent;
}
.file-row:hover { background: var(--panel); }
.file-row.active { background: var(--panel-2); border-color: #344554; }
.file-icon {
  flex: 0 0 36px; height: 36px; border-radius: 8px; display: grid; place-items: center;
  font-size: 9px; font-weight: 700; letter-spacing: 0.04em;
  color: #08110d; background: linear-gradient(135deg, var(--accent), var(--accent-2));
}
.file-icon.dir { color: #1d1605; background: linear-gradient(135deg, var(--dir), #e0a33a); }
.file-main { min-width: 0; flex: 1; }
.file-name { font-weight: 560; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.file-sub { color: var(--muted); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.preview { flex: 1; overflow: auto; padding: 30px clamp(22px, 5vw, 64px) 80px; min-height: 0; }
.empty { color: var(--muted); margin-top: 18vh; text-align: center; }
.preview-head { display: flex; gap: 18px; align-items: center; padding-bottom: 20px; border-bottom: 1px solid var(--line); }
.preview-head .file-icon { flex-basis: 64px; height: 64px; font-size: 13px; border-radius: 13px; }
h1 { margin: 0; font-size: clamp(22px, 3vw, 34px); line-height: 1.12; letter-spacing: -0.02em; overflow-wrap: anywhere; }
.path { color: var(--muted); font-size: 12px; margin-top: 6px; overflow-wrap: anywhere; }
.actions { margin-left: auto; display: flex; gap: 7px; flex-wrap: wrap; justify-content: flex-end; }
.meta-grid { display: flex; flex-wrap: wrap; gap: 12px 22px; margin: 24px 0; }
.meta-item { flex: 1 1 220px; border-bottom: 1px dashed #1d262e; padding-bottom: 7px; min-width: 0; }
.meta-label { color: var(--muted); font-size: 10px; text-transform: uppercase; letter-spacing: 0.12em; }
.meta-value { overflow-wrap: anywhere; font-size: 13px; margin-top: 2px; }
.preview-body { margin-top: 22px; }
.preview-body img, .preview-body video { max-width: 100%; max-height: 70vh; border-radius: 10px; border: 1px solid var(--line); }
.preview-body audio { width: 100%; }
.preview-body iframe { width: 100%; height: 70vh; border: 1px solid var(--line); border-radius: 10px; background: #fff; }
pre {
  white-space: pre-wrap; overflow-wrap: anywhere; font-size: 12px; line-height: 1.5;
  background: #080b0e; border: 1px solid var(--line); border-radius: 10px;
  padding: 15px; color: #b9c7d1; max-height: 68vh; overflow: auto;
}
.notice { color: var(--muted); background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 14px 16px; }
@media (max-width: 520px) {
  main { flex-direction: column; height: auto; }
  aside { flex: 0 0 auto; border-right: 0; border-bottom: 1px solid var(--line); max-height: 50vh; }
  .preview { padding: 24px 18px 70px; }
}
</style>
</head>
<body>
<header>
  <div class="brand"><span class="dot"></span><span>Soil VFS</span></div>
  <div class="status" id="status">Loading workspace…</div>
  <div class="spacer"></div>
  <button class="button" id="refresh">Refresh</button>
</header>
<main>
  <aside>
    <div class="toolbar">
      <button class="button" id="up" title="Parent directory">↑</button>
      <div class="crumb" id="crumb"></div>
    </div>
    <div class="searchbar">
      <input id="search" type="search" placeholder="Filter or search files…" autocomplete="off" spellcheck="false">
      <select id="mode" title="Search mode">
        <option value="substring">substring</option>
        <option value="fuzzy">fuzzy</option>
        <option value="full-text">full-text</option>
        <option value="semantic">semantic</option>
      </select>
    </div>
    <div class="count" id="count"></div>
    <ul class="file-list" id="list"></ul>
  </aside>
  <section class="preview" id="preview"><div class="empty">Select a file</div></section>
</main>
<script>
const state = { path: "", entries: [], selected: null, query: "", searching: false };
const $ = (id) => document.getElementById(id);

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function extOf(name) {
  const base = String(name || "").split("/").pop();
  const dot = base.lastIndexOf(".");
  if (dot <= 0 || dot === base.length - 1) return "";
  return base.slice(dot + 1).toUpperCase().slice(0, 4);
}

function iconLabel(entry) {
  if (entry.kind === "directory") return "DIR";
  const ext = extOf(entry.name);
  if (ext) return ext;
  const type = entry.media_type || "";
  if (type.startsWith("image/")) return "IMG";
  if (type.startsWith("audio/")) return "AUD";
  if (type.startsWith("video/")) return "VID";
  if (type.startsWith("text/")) return "TXT";
  return "BIN";
}

function formatSize(bytes) {
  const n = Number(bytes) || 0;
  if (n < 1024) return n + " B";
  const units = ["KB", "MB", "GB", "TB"];
  let value = n / 1024, unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return value.toFixed(value >= 10 ? 0 : 1) + " " + units[unit];
}

function formatDate(ms) {
  return ms ? new Date(ms).toLocaleString() : "—";
}

function contentUrl(path, download) {
  return "/api/content?path=" + encodeURIComponent(path) + (download ? "&download=1" : "");
}

function navigate(path) {
  state.path = path || "";
  state.query = "";
  state.searching = false;
  state.selected = null;
  $("search").value = "";
  loadEntries();
}

function up() {
  if (!state.path) return;
  const parts = state.path.split("/");
  parts.pop();
  navigate(parts.join("/"));
}

function renderCrumb() {
  const crumb = $("crumb");
  crumb.replaceChildren();
  const root = el("button", null, "VFS");
  root.addEventListener("click", () => navigate(""));
  crumb.append(root);
  let current = "";
  for (const part of state.path.split("/").filter(Boolean)) {
    current = current ? current + "/" + part : part;
    crumb.append(el("span", null, "/"));
    const button = el("button", null, part);
    const target = current;
    button.addEventListener("click", () => navigate(target));
    crumb.append(button);
  }
}

function renderList() {
  const list = $("list");
  list.replaceChildren();
  const fragment = document.createDocumentFragment();
  for (const entry of state.entries) {
    const row = el("li", "file-row" + (state.selected && state.selected.path === entry.path ? " active" : ""));
    row.tabIndex = 0;
    row.append(el("div", "file-icon" + (entry.kind === "directory" ? " dir" : ""), iconLabel(entry)));
    const main = el("div", "file-main");
    main.append(el("div", "file-name", entry.name || entry.path || "/"));
    let sub;
    if (state.searching) sub = entry.path;
    else if (entry.kind === "directory") sub = (entry.child_count || 0) + " items";
    else sub = formatSize(entry.size) + " · " + formatDate(entry.updated_at_ms);
    main.append(el("div", "file-sub", sub));
    row.append(main);
    row.addEventListener("click", () => select(entry));
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        select(entry);
      }
    });
    fragment.append(row);
  }
  list.append(fragment);
  $("count").textContent = state.searching ? state.entries.length + " matches" : state.entries.length + " entries";
  $("up").disabled = !state.path;
}

function metaItem(label, value) {
  const item = el("div", "meta-item");
  item.append(el("div", "meta-label", label));
  item.append(el("div", "meta-value", value));
  return item;
}

function select(entry) {
  state.selected = entry;
  renderList();
  if (entry.kind === "directory") { navigate(entry.path); return; }
  showFile(entry);
}

function showFile(entry) {
  const preview = $("preview");
  preview.replaceChildren();
  const head = el("div", "preview-head");
  head.append(el("div", "file-icon", iconLabel(entry)));
  const titleWrap = el("div");
  titleWrap.append(el("h1", null, entry.name || entry.path));
  titleWrap.append(el("div", "path", entry.path));
  head.append(titleWrap);
  const actions = el("div", "actions");
  const raw = el("a", "button", "Open raw");
  raw.href = contentUrl(entry.path, false);
  raw.target = "_blank";
  const download = el("a", "button", "Download");
  download.href = contentUrl(entry.path, true);
  download.download = entry.name;
  actions.append(raw, download);
  head.append(actions);
  preview.append(head);

  const grid = el("div", "meta-grid");
  grid.append(
    metaItem("Size", formatSize(entry.size)),
    metaItem("Media type", entry.media_type || "application/octet-stream"),
    metaItem("Updated", formatDate(entry.updated_at_ms)),
    metaItem("Version", String(entry.version)),
    metaItem("Hydration", entry.hydration),
    metaItem("Pinned", entry.pinned ? "yes" : "no"),
  );
  grid.append(metaItem("Content ID", entry.content_id || "—"));
  preview.append(grid);

  const body = el("div", "preview-body");
  const type = entry.media_type || "";
  if (type.startsWith("image/")) {
    const img = el("img");
    img.src = contentUrl(entry.path, false);
    img.alt = entry.name;
    body.append(img);
  } else if (type === "application/pdf") {
    const frame = el("iframe");
    frame.src = contentUrl(entry.path, false);
    frame.title = entry.name;
    body.append(frame);
  } else if (type.startsWith("audio/")) {
    const audio = el("audio");
    audio.controls = true;
    audio.src = contentUrl(entry.path, false);
    body.append(audio);
  } else if (type.startsWith("video/")) {
    const video = el("video");
    video.controls = true;
    video.src = contentUrl(entry.path, false);
    body.append(video);
  } else if (type.startsWith("text/") || type === "application/json" || type === "application/xml" || /[.](md|txt|json|xml|ya?ml|toml|csv|rs|js|ts|py|sh|css|html)$/i.test(entry.name)) {
    const pre = el("pre", null, "Loading preview…");
    body.append(pre);
    fetch(contentUrl(entry.path, false), { cache: "no-store" })
      .then((response) => response.ok ? response.text() : Promise.reject(new Error("HTTP " + response.status)))
      .then((text) => { pre.textContent = text.length > 400000 ? text.slice(0, 400000) + "\n\n… preview truncated" : text; })
      .catch((error) => { pre.textContent = String(error); });
  } else {
    body.append(el("div", "notice", "No inline preview for " + (type || "this file type") + ". Open raw or download it."));
  }
  preview.append(body);
}

async function loadEntries() {
  try {
    const response = await fetch("/api/entries?path=" + encodeURIComponent(state.path), { cache: "no-store" });
    if (!response.ok) throw new Error("HTTP " + response.status);
    const data = await response.json();
    state.entries = data.entries || [];
    state.searching = false;
    state.selected = null;
    $("status").textContent = data.root + (data.index && data.index.entries ? " · indexed " + data.index.entries + " entries" : "");
    renderCrumb();
    renderList();
    if (!state.entries.length) {
      $("preview").replaceChildren(el("div", "empty", state.path ? "This directory is empty" : "The VFS root is empty"));
    } else if (!state.path) {
      $("preview").replaceChildren(el("div", "empty", "Select a file"));
    }
  } catch (error) {
    $("status").textContent = "Failed to load workspace";
    $("preview").replaceChildren(el("div", "empty", String(error)));
  }
}

async function doSearch() {
  const query = $("search").value.trim();
  if (!query) { loadEntries(); return; }
  try {
    const mode = $("mode").value;
    const response = await fetch("/api/search?q=" + encodeURIComponent(query) + "&mode=" + encodeURIComponent(mode), { cache: "no-store" });
    if (!response.ok) throw new Error("HTTP " + response.status);
    const data = await response.json();
    state.entries = (data.hits || []).map((hit) => hit.entry);
    state.searching = true;
    state.selected = null;
    $("count").textContent = state.entries.length + " matches";
    renderList();
    $("preview").replaceChildren(el("div", "empty", state.entries.length ? "Select a result" : "No matches"));
  } catch (error) {
    $("preview").replaceChildren(el("div", "empty", String(error)));
  }
}

let timer;
$("search").addEventListener("input", () => {
  clearTimeout(timer);
  timer = setTimeout(doSearch, 180);
});
$("mode").addEventListener("change", doSearch);
$("refresh").addEventListener("click", () => state.query ? doSearch() : loadEntries());
$("up").addEventListener("click", up);
loadEntries();
</script>
</body>
</html>
"#;
