//! Basic browser inspector for a `fungos-records` contact book served through
//! the `soil:records` host adapter.
//!
//! This is deliberately a small local tool, not a production web service:
//!
//! ```sh
//! cargo run -p soil-records-host --bin soil-records-web -- \
//!   --book personal/avery/contacts --collection contact
//! ```
//!
//! It opens the same SQLite book as `records`, reads through
//! `RecordsBookHost`, and serves read-only HTML/JSON over loopback.

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
use soil_records_host::{RecordsBookHost, records::Entry};

const DEFAULT_ROOT: &str = "~/.local/share/fungos/records";
const DEFAULT_BOOK: &str = "personal/avery/contacts";
const DEFAULT_COLLECTION: &str = "contact";
const DEFAULT_LISTEN: &str = "127.0.0.1:8787";
const MAX_ENTRIES: usize = 10_000;

struct App {
    host: Mutex<RecordsBookHost>,
    book: String,
    collection: String,
}

struct Args {
    root: PathBuf,
    book: String,
    collection: String,
    listen: String,
}

fn main() -> Result<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let host = RecordsBookHost::open_contacts(&args.root, &args.book, &args.collection)
        .with_context(|| format!("open {}", args.root.display()))?;
    let app = Arc::new(App {
        host: Mutex::new(host),
        book: args.book.clone(),
        collection: args.collection.clone(),
    });
    let listener =
        TcpListener::bind(&args.listen).with_context(|| format!("bind {}", args.listen))?;
    println!(
        "soil contacts: http://{}/  book={} collection={} root={}",
        args.listen,
        args.book,
        args.collection,
        args.root.display()
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                thread::spawn(move || {
                    if let Err(error) = handle(stream, &app) {
                        eprintln!("soil contacts: request failed: {error:#}");
                    }
                });
            }
            Err(error) => eprintln!("soil contacts: accept failed: {error}"),
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
        "/api/records" => {
            let records = all_entries(app)?;
            let q = query_param(query, "q").unwrap_or_default().to_lowercase();
            let mut rows: Vec<Value> = records
                .iter()
                .filter(|entry| q.is_empty() || search_text(entry).contains(&q))
                .map(entry_json)
                .collect();
            rows.sort_by_key(|row| display_name(row).unwrap_or_default().to_lowercase());
            let body = json!({
                "book": app.book,
                "collection": app.collection,
                "total": rows.len(),
                "records": rows,
            });
            respond_json(&mut stream, "200 OK", &body)
        }
        "/api/export.vcf" => {
            let entries = all_entries(app)?;
            let mut vcf = String::new();
            for entry in &entries {
                let body = body_json(entry);
                if let Some(raw) = body.get("raw_vcard").and_then(Value::as_str)
                    && !raw.is_empty()
                {
                    vcf.push_str(raw);
                    if !vcf.ends_with('\n') {
                        vcf.push('\n');
                    }
                }
            }
            stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/vcard; charset=utf-8\r\nContent-Length: {}\r\nContent-Disposition: attachment; filename=\"contacts.vcf\"\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                    vcf.len()
                )
                .as_bytes(),
            )?;
            stream.write_all(vcf.as_bytes())?;
            Ok(())
        }
        path if path.starts_with("/api/records/") => {
            let id = percent_decode(&path["/api/records/".len()..]);
            let entry = {
                let mut host = app.host.lock().expect("records host poisoned");
                host.get_entry(&id)?
            };
            match entry {
                Some(entry) => respond_json(&mut stream, "200 OK", &entry_json(&entry)),
                None => respond(
                    &mut stream,
                    "404 Not Found",
                    "application/json",
                    br#"{"error":"not found"}"#,
                ),
            }
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

fn all_entries(app: &App) -> Result<Vec<Entry>> {
    let mut host = app.host.lock().expect("records host poisoned");
    let entries = host.scan_all(MAX_ENTRIES)?;
    drop(host);
    Ok(entries)
}

fn body_json(entry: &Entry) -> Value {
    serde_json::from_slice(&entry.body).unwrap_or(Value::Null)
}

fn entry_json(entry: &Entry) -> Value {
    json!({
        "id": entry.key.id,
        "collection": entry.key.collection,
        "schema": entry.schema,
        "revision": entry.revision,
        "updated_at_ms": entry.updated_at_ms,
        "origin": entry.origin,
        "deleted": entry.deleted,
        "body": body_json(entry),
    })
}

fn display_name(record: &Value) -> Option<String> {
    record
        .get("body")
        .and_then(|body| body.get("display_name"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn search_text(entry: &Entry) -> String {
    let body = body_json(entry);
    let mut parts = Vec::new();
    for key in [
        "display_name",
        "given_name",
        "family_name",
        "organization",
        "title",
        "note",
        "birthday",
    ] {
        if let Some(value) = body.get(key).and_then(Value::as_str) {
            parts.push(value);
        }
    }
    for key in ["emails", "phones", "addresses", "urls"] {
        if let Some(values) = body.get(key).and_then(Value::as_array) {
            for value in values {
                if let Some(value) = value.get("value").and_then(Value::as_str) {
                    parts.push(value);
                }
            }
        }
    }
    parts.join(" ").to_lowercase()
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

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if percent_decode(key) == name {
            Some(percent_decode(value).replace('+', " "))
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
        book: DEFAULT_BOOK.into(),
        collection: DEFAULT_COLLECTION.into(),
        listen: DEFAULT_LISTEN.into(),
    };
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--root" => {
                args.root =
                    PathBuf::from(expand_home(&iter.next().context("--root needs a value")?));
            }
            "--book" => args.book = iter.next().context("--book needs a value")?,
            "--collection" => {
                args.collection = iter.next().context("--collection needs a value")?
            }
            "--listen" => args.listen = iter.next().context("--listen needs a value")?,
            "-h" | "--help" => {
                println!(
                    "soil-records-web [--root DIR] [--book BOOK] [--collection NAME] [--listen ADDR]\n\
                     defaults: root={DEFAULT_ROOT} book={DEFAULT_BOOK} collection={DEFAULT_COLLECTION} listen={DEFAULT_LISTEN}"
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
<title>Soil Contacts</title>
<style>
:root {
  color-scheme: dark;
  --bg: #0b0e11;
  --panel: #12171c;
  --panel-2: #171e25;
  --line: #26313b;
  --text: #dce5ea;
  --muted: #8395a2;
  --accent: #7dd3a8;
  --accent-2: #5bb8ff;
}
* { box-sizing: border-box; }
html, body { height: 100%; }
body {
  margin: 0;
  background: radial-gradient(circle at 20% -10%, #16232a 0, var(--bg) 45%);
  color: var(--text);
  font: 15px/1.45 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
header {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 14px 20px;
  border-bottom: 1px solid var(--line);
  background: rgba(11, 14, 17, 0.86);
  backdrop-filter: blur(12px);
  position: sticky;
  top: 0;
  z-index: 2;
}
.brand { display: flex; align-items: center; gap: 9px; font-weight: 650; letter-spacing: 0.02em; }
.dot {
  width: 10px; height: 10px; border-radius: 50%;
  background: var(--accent);
  box-shadow: 0 0 14px rgba(125, 211, 168, 0.8);
}
.status { color: var(--muted); font-size: 13px; }
header .spacer { flex: 1; }
.button {
  color: var(--text); text-decoration: none; font-size: 13px;
  border: 1px solid var(--line); background: var(--panel-2);
  border-radius: 8px; padding: 6px 11px;
}
.button:hover { border-color: #3b4c5c; }
main { display: grid; grid-template-columns: minmax(240px, 340px) 1fr; height: calc(100vh - 59px); }
aside { border-right: 1px solid var(--line); min-height: 0; display: flex; flex-direction: column; }
.search { padding: 14px 14px 8px; }
input {
  width: 100%; border: 1px solid var(--line); background: var(--panel);
  color: var(--text); border-radius: 9px; padding: 10px 12px; font: inherit; outline: none;
}
input:focus { border-color: var(--accent-2); box-shadow: 0 0 0 3px rgba(91, 184, 255, 0.12); }
.count { padding: 2px 16px 9px; color: var(--muted); font-size: 12px; }
.contact-list { list-style: none; padding: 0 8px 20px; margin: 0; overflow: auto; min-height: 0; }
.contact-row {
  display: flex; gap: 11px; align-items: center; padding: 9px 10px;
  border-radius: 9px; cursor: pointer; border: 1px solid transparent;
}
.contact-row:hover { background: var(--panel); }
.contact-row.active { background: var(--panel-2); border-color: #344554; }
.avatar {
  flex: 0 0 34px; height: 34px; border-radius: 50%;
  display: grid; place-items: center; font-size: 12px; font-weight: 700;
  color: #07110c; background: linear-gradient(135deg, var(--accent), var(--accent-2));
}
.row-main { min-width: 0; }
.row-name { font-weight: 560; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.row-sub { color: var(--muted); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.detail { overflow: auto; padding: 30px clamp(22px, 5vw, 64px) 80px; min-height: 0; }
.empty { color: var(--muted); margin-top: 18vh; text-align: center; }
.detail-head { display: flex; gap: 20px; align-items: center; padding-bottom: 22px; border-bottom: 1px solid var(--line); }
.avatar.large { flex-basis: 68px; height: 68px; font-size: 22px; }
h1 { margin: 0; font-size: clamp(24px, 3vw, 36px); line-height: 1.12; letter-spacing: -0.02em; }
.subtitle { color: var(--muted); margin-top: 5px; }
.section { margin-top: 26px; }
.section-title {
  margin: 0 0 9px; font-size: 11px; letter-spacing: 0.14em;
  text-transform: uppercase; color: var(--muted); font-weight: 650;
}
.field { display: flex; gap: 10px; align-items: baseline; padding: 7px 0; border-bottom: 1px dashed #1d262e; }
.field:last-child { border-bottom: 0; }
.tag {
  flex: 0 0 auto; color: var(--accent); background: rgba(125, 211, 168, 0.08);
  border: 1px solid rgba(125, 211, 168, 0.22); border-radius: 999px;
  font-size: 10px; padding: 1px 7px; text-transform: lowercase;
}
.value { white-space: pre-wrap; overflow-wrap: anywhere; }
a.link { color: var(--accent-2); text-decoration: none; }
a.link:hover { text-decoration: underline; }
.note { white-space: pre-wrap; background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 13px 15px; }
.raw { margin-top: 34px; border-top: 1px solid var(--line); padding-top: 18px; }
.raw summary { cursor: pointer; color: var(--muted); font-size: 13px; }
pre {
  white-space: pre-wrap; overflow-wrap: anywhere; font-size: 12px; line-height: 1.5;
  background: #080b0e; border: 1px solid var(--line); border-radius: 10px;
  padding: 14px; color: #b9c7d1; max-height: 420px; overflow: auto;
}
.meta { color: var(--muted); font-size: 11px; margin-top: 22px; display: flex; flex-wrap: wrap; gap: 8px 18px; }
@media (max-width: 620px) {
  main { grid-template-columns: 1fr; height: auto; }
  aside { border-right: 0; border-bottom: 1px solid var(--line); max-height: 48vh; }
  .detail { padding: 24px 18px 70px; }
}
</style>
</head>
<body>
<header>
  <div class="brand"><span class="dot"></span>Soil Contacts</div>
  <div class="status" id="status">Loading records…</div>
  <div class="spacer"></div>
  <a class="button" href="/api/export.vcf" download="contacts.vcf">Export .vcf</a>
</header>
<main>
  <aside>
    <div class="search"><input id="search" type="search" placeholder="Search name, org, email, phone…" autocomplete="off" spellcheck="false"></div>
    <div class="count" id="count"></div>
    <ul class="contact-list" id="list"></ul>
  </aside>
  <section class="detail" id="detail"><div class="empty">Select a contact</div></section>
</main>
<script>
const state = { records: [], filtered: [], selected: null };

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function initials(name) {
  const parts = String(name || "").trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return "?";
  return (parts[0][0] + (parts.length > 1 ? parts[parts.length - 1][0] : "")).toUpperCase();
}

function searchText(record) {
  const b = record.body || {};
  const values = [b.display_name, b.given_name, b.family_name, b.organization, b.title, b.note, b.birthday];
  for (const key of ["emails", "phones", "addresses", "urls"]) {
    for (const item of (b[key] || [])) values.push(item.value);
  }
  return values.filter(Boolean).join(" ").toLowerCase();
}

function renderList() {
  const list = document.getElementById("list");
  list.replaceChildren();
  const fragment = document.createDocumentFragment();
  for (const record of state.filtered) {
    const b = record.body || {};
    const row = el("li", "contact-row" + (state.selected && state.selected.id === record.id ? " active" : ""));
    row.tabIndex = 0;
    const avatar = el("div", "avatar", initials(b.display_name));
    const main = el("div", "row-main");
    main.append(el("div", "row-name", b.display_name || record.id));
    const org = [b.organization, b.title].filter(Boolean).join(" · ");
    const email = (b.emails || [])[0] && b.emails[0].value;
    main.append(el("div", "row-sub", org || email || b.note || record.id));
    row.append(avatar, main);
    row.addEventListener("click", () => select(record));
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        select(record);
      }
    });
    fragment.append(row);
  }
  list.append(fragment);
  document.getElementById("count").textContent =
    state.filtered.length === state.records.length
      ? state.records.length + " contacts"
      : state.filtered.length + " of " + state.records.length + " contacts";
}

function fieldRow(label, value, kind) {
  const row = el("div", "field");
  if (label) row.append(el("span", "tag", label));
  let node = el("span", "value", value);
  if (kind === "email" || kind === "phone" || kind === "url") {
    const link = el("a", "link", value);
    link.href = kind === "email" ? "mailto:" + value : kind === "phone" ? "tel:" + value : value;
    if (kind === "url") link.target = "_blank";
    node = link;
  }
  row.append(node);
  return row;
}

function section(title, items, kind) {
  if (!items || !items.length) return null;
  const sec = el("section", "section");
  sec.append(el("h2", "section-title", title));
  for (const item of items) sec.append(fieldRow(item.label, item.value, kind));
  return sec;
}

function select(record) {
  state.selected = record;
  renderList();
  const detail = document.getElementById("detail");
  detail.replaceChildren();
  const b = record.body || {};

  const head = el("div", "detail-head");
  head.append(el("div", "avatar large", initials(b.display_name)));
  const titleWrap = el("div");
  titleWrap.append(el("h1", null, b.display_name || record.id));
  const subtitle = [b.title, b.organization].filter(Boolean).join(" · ");
  if (subtitle) titleWrap.append(el("div", "subtitle", subtitle));
  head.append(titleWrap);
  detail.append(head);

  for (const sec of [
    section("Email", b.emails, "email"),
    section("Phone", b.phones, "phone"),
    section("Address", b.addresses, "text"),
    section("URL", b.urls, "url"),
  ]) {
    if (sec) detail.append(sec);
  }

  if (b.birthday) {
    const sec = el("section", "section");
    sec.append(el("h2", "section-title", "Birthday"));
    sec.append(fieldRow("", b.birthday, "text"));
    detail.append(sec);
  }

  if (b.note) {
    const sec = el("section", "section");
    sec.append(el("h2", "section-title", "Note"));
    sec.append(el("div", "note", b.note));
    detail.append(sec);
  }

  if (b.raw_vcard) {
    const raw = el("details", "raw");
    raw.append(el("summary", null, "Raw vCard"));
    const pre = el("pre");
    pre.textContent = b.raw_vcard;
    raw.append(pre);
    detail.append(raw);
  }

  const meta = el("div", "meta");
  meta.append(el("span", null, "id " + record.id));
  if (b.uid) meta.append(el("span", null, "uid " + b.uid));
  if (record.updated_at_ms) meta.append(el("span", null, "updated " + new Date(record.updated_at_ms).toLocaleString()));
  if (record.origin) meta.append(el("span", null, "origin " + record.origin));
  if (record.revision) meta.append(el("span", null, "rev " + record.revision));
  detail.append(meta);
}

function applyFilter() {
  const query = document.getElementById("search").value.trim().toLowerCase();
  state.filtered = query
    ? state.records.filter((record) => searchText(record).includes(query))
    : state.records.slice();
  renderList();
  if (state.selected && !state.filtered.some((record) => record.id === state.selected.id)) {
    // Keep the detail open: filtering is a list operation, not a selection reset.
  }
}

async function load() {
  try {
    const response = await fetch("/api/records", { cache: "no-store" });
    if (!response.ok) throw new Error("HTTP " + response.status);
    const data = await response.json();
    state.records = data.records || [];
    state.filtered = state.records.slice();
    document.getElementById("status").textContent =
      data.book + " / " + data.collection;
    renderList();
    if (state.records.length) select(state.records[0]);
  } catch (error) {
    document.getElementById("status").textContent = "Failed to load records";
    document.getElementById("detail").replaceChildren(el("div", "empty", String(error)));
  }
}

let timer;
document.getElementById("search").addEventListener("input", () => {
  clearTimeout(timer);
  timer = setTimeout(applyFilter, 90);
});
load();
</script>
</body>
</html>
"#;
