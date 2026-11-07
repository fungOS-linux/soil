//! Read-only discovery UI for `soil:search`.
//!
//! Sources are opt-in host arguments: VFS, records books, Git roots, Spore
//! roots, and Mycelium peer/service snapshots. The browser can only ask for
//! kinds the host already configured.

use std::{
    env,
    io::{self, BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context, Result};
use fungos_records::Store;
use serde_json::{Value, json};
use soil_search_host::api::Host as _;
use soil_search_host::mention_api::Host as _;
use soil_search_host::{SearchService, api};
use soil_vfs_host::VfsHost;

const DEFAULT_LISTEN: &str = "127.0.0.1:8790";
const MAX_PAGE: u32 = 128;
struct App {
    service: Mutex<SearchService>,
    sources: u32,
}

#[derive(Default)]
struct Args {
    listen: Option<String>,
    vfs_root: Option<PathBuf>,
    records_root: Option<PathBuf>,
    books: Vec<String>,
    git_roots: Vec<PathBuf>,
    spore_roots: Vec<PathBuf>,
    mycelium_snapshot: Option<PathBuf>,
    applications_snapshot: Option<PathBuf>,
    marbles_snapshot: Option<PathBuf>,
    marbles_store: Option<String>,
    marbles_company: Option<String>,
    principals_snapshot: Option<PathBuf>,
}

fn main() -> Result<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let vfs = match &args.vfs_root {
        Some(root) => {
            Some(VfsHost::open(root).with_context(|| format!("open VFS {}", root.display()))?)
        }
        None => None,
    };
    let records = match &args.records_root {
        Some(root) => {
            let device = root.join("device_id");
            Some(
                Store::open(root, &device)
                    .with_context(|| format!("open records {}", root.display()))?,
            )
        }
        None => None,
    };
    let mut service = SearchService::new(vfs, records, args.books.clone());
    if !args.git_roots.is_empty() {
        service = service.with_git_roots(args.git_roots.clone());
    }
    if !args.spore_roots.is_empty() {
        service = service.with_spore_roots(args.spore_roots.clone());
    }
    if let Some(dir) = &args.mycelium_snapshot {
        service = service.with_mycelium_snapshot_dir(dir);
    }
    if let Some(path) = &args.applications_snapshot {
        service = service.with_applications_snapshot(path);
    }
    if let Some(path) = &args.marbles_snapshot {
        service = service.with_marbles(soil_marbles::MarblesIndex::from_snapshot(
            path,
            args.marbles_store.as_deref().unwrap_or("store"),
            args.marbles_company.as_deref().unwrap_or("fpl"),
        )?);
    }
    if let Some(path) = &args.principals_snapshot {
        let bytes = std::fs::read(path)
            .with_context(|| format!("read principal snapshot {}", path.display()))?;
        let principals: Vec<soil_search_host::PrincipalCandidate> =
            serde_json::from_slice(&bytes).context("parse principal snapshot")?;
        service = service.with_principals(principals);
    }
    let sources = service.state().map(|state| state.sources).unwrap_or(0);
    let app = Arc::new(App {
        service: Mutex::new(service),
        sources,
    });
    let listen = args.listen.unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    let listener = TcpListener::bind(&listen).with_context(|| format!("bind {listen}"))?;
    println!("soil search: http://{listen}/  sources={sources}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                thread::spawn(move || {
                    if let Err(error) = handle(stream, &app) {
                        eprintln!("soil search: request failed: {error:#}");
                    }
                });
            }
            Err(error) => eprintln!("soil search: accept failed: {error}"),
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
        "/api/search" => {
            let text = query_param(query, "q").unwrap_or_default();
            if text.trim().is_empty() {
                return respond_json(
                    &mut stream,
                    "200 OK",
                    &json!({"query": "", "hits": [], "next": null, "sources": app.sources}),
                );
            }
            let kinds = parse_kinds(query_param(query, "kinds").unwrap_or_default());
            let mode = parse_mode(query_param(query, "mode").unwrap_or_default());
            let offset = query_param(query, "offset")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let result = {
                let mut service = app.service.lock().expect("search service poisoned");
                service.search(api::Query {
                    text,
                    kinds,
                    mode,
                    offset,
                    limit: MAX_PAGE,
                })
            };
            match result {
                Ok(page) => respond_json(
                    &mut stream,
                    "200 OK",
                    &json!({
                        "hits": page.hits.into_iter().map(hit_json).collect::<Vec<_>>(),
                        "next": page.next,
                        "generation": page.generation,
                        "sources": app.sources,
                    }),
                ),
                Err(error) => respond_json(
                    &mut stream,
                    "400 Bad Request",
                    &json!({"error": format!("{error:?}")}),
                ),
            }
        }
        "/api/suggest" => {
            let text = query_param(query, "q").unwrap_or_default();
            if text.trim().is_empty() {
                return respond_json(
                    &mut stream,
                    "200 OK",
                    &json!({"query": "", "candidates": []}),
                );
            }
            let kinds = query_param(query, "kinds")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|kind| !kind.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            let limit = query_param(query, "limit")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(10)
                .clamp(1, 64);
            let result = {
                let mut service = app.service.lock().expect("search service poisoned");
                service.suggest(text, kinds, limit)
            };
            match result {
                Ok(candidates) => respond_json(
                    &mut stream,
                    "200 OK",
                    &json!({
                        "candidates": candidates.into_iter().map(mention_json).collect::<Vec<_>>(),
                    }),
                ),
                Err(error) => respond_json(
                    &mut stream,
                    "400 Bad Request",
                    &json!({"error": format!("{error:?}")}),
                ),
            }
        }
        "/api/refresh" => {
            let state = {
                let mut service = app.service.lock().expect("search service poisoned");
                service.refresh()
            };
            match state {
                Ok(state) => respond_json(
                    &mut stream,
                    "200 OK",
                    &json!({"ready": state.ready, "generation": state.generation, "sources": state.sources}),
                ),
                Err(error) => respond_json(
                    &mut stream,
                    "500 Internal Server Error",
                    &json!({"error": format!("{error:?}")}),
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

fn hit_json(hit: api::Hit) -> Value {
    json!({
        "kind": kind_name(hit.kind),
        "id": hit.id,
        "title": hit.title,
        "subtitle": hit.subtitle,
        "snippet": hit.snippet,
        "score": hit.score,
        "source": hit.source,
        "updated_at_ms": hit.updated_at_ms,
        "references": hit.references.into_iter().map(|reference| json!({
            "kind": reference.kind,
            "id": reference.id,
            "rel": reference.rel,
            "title": reference.title,
        })).collect::<Vec<_>>(),
    })
}

fn mention_json(candidate: soil_search_host::mention_api::Candidate) -> Value {
    json!({
        "kind": candidate.kind,
        "token": candidate.token,
        "label": candidate.label,
        "subtitle": candidate.subtitle,
        "schema": candidate.schema,
        "format": candidate.format,
        "score": candidate.score,
        "visibility": candidate.visibility,
    })
}

fn kind_name(kind: api::Kind) -> &'static str {
    match kind {
        api::Kind::File => "file",
        api::Kind::RecordEntry => "record-entry",
        api::Kind::Repository => "repository",
        api::Kind::Spore => "spore",
        api::Kind::Peer => "peer",
        api::Kind::Service => "service",
        api::Kind::Application => "application",
        api::Kind::Action => "action",
        api::Kind::Task => "task",
        api::Kind::Principal => "principal",
    }
}

fn parse_kinds(value: String) -> Vec<api::Kind> {
    value
        .split(',')
        .filter_map(|kind| match kind.trim() {
            "file" => Some(api::Kind::File),
            "record-entry" => Some(api::Kind::RecordEntry),
            "repository" => Some(api::Kind::Repository),
            "spore" => Some(api::Kind::Spore),
            "peer" => Some(api::Kind::Peer),
            "service" => Some(api::Kind::Service),
            "application" => Some(api::Kind::Application),
            "action" => Some(api::Kind::Action),
            "task" => Some(api::Kind::Task),
            "principal" => Some(api::Kind::Principal),
            _ => None,
        })
        .collect()
}

fn parse_mode(value: String) -> Option<api::Mode> {
    match value.as_str() {
        "fuzzy" => Some(api::Mode::Fuzzy),
        "full-text" => Some(api::Mode::FullText),
        "semantic" => Some(api::Mode::Semantic),
        "regex" => Some(api::Mode::Regex),
        "substring" => Some(api::Mode::Substring),
        _ => None,
    }
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
    let mut args = Args::default();
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--listen" => args.listen = Some(iter.next().context("--listen needs a value")?),
            "--vfs-root" => {
                args.vfs_root = Some(PathBuf::from(
                    iter.next().context("--vfs-root needs a value")?,
                ));
            }
            "--records-root" => {
                args.records_root = Some(PathBuf::from(
                    iter.next().context("--records-root needs a value")?,
                ));
            }
            "--book" => args
                .books
                .push(iter.next().context("--book needs a value")?),
            "--git-root" => {
                args.git_roots.push(PathBuf::from(
                    iter.next().context("--git-root needs a value")?,
                ));
            }
            "--spore-root" => {
                args.spore_roots.push(PathBuf::from(
                    iter.next().context("--spore-root needs a value")?,
                ));
            }
            "--mycelium-snapshot" => {
                args.mycelium_snapshot = Some(PathBuf::from(
                    iter.next().context("--mycelium-snapshot needs a value")?,
                ));
            }
            "--applications-snapshot" => {
                args.applications_snapshot = Some(PathBuf::from(
                    iter.next().context("--applications-snapshot needs a value")?,
                ));
            }
            "--marbles-snapshot" => {
                args.marbles_snapshot = Some(PathBuf::from(
                    iter.next().context("--marbles-snapshot needs a value")?,
                ));
            }
            "--marbles-store" => {
                args.marbles_store = Some(iter.next().context("--marbles-store needs a value")?);
            }
            "--marbles-company" => {
                args.marbles_company =
                    Some(iter.next().context("--marbles-company needs a value")?);
            }
            "--principals-snapshot" => {
                args.principals_snapshot = Some(PathBuf::from(
                    iter.next().context("--principals-snapshot needs a value")?,
                ));
            }
            "-h" | "--help" => {
                println!(
                    "soil-search-web [--listen ADDR] [--vfs-root DIR] [--records-root DIR] [--book BOOK]...\\n\
                     \\t[--git-root DIR]... [--spore-root DIR]... [--mycelium-snapshot DIR]\\n\
                     default: http://{DEFAULT_LISTEN}/"
                );
                return Ok(None);
            }
            other => anyhow::bail!("unknown argument {other}; try --help"),
        }
    }
    Ok(Some(args))
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Soil Search</title>
<style>
:root {
  color-scheme: dark;
  --bg: #0b0e11; --panel: #12171c; --panel-2: #171e25; --line: #26313b;
  --text: #dce5ea; --muted: #8395a2; --accent: #7dd3a8; --accent-2: #5bb8ff;
}
* { box-sizing: border-box; }
body {
  margin: 0; background: radial-gradient(circle at 20% -10%, #16232a 0, var(--bg) 45%);
  color: var(--text); font: 15px/1.45 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
header { display: flex; align-items: center; gap: 16px; padding: 14px 20px; border-bottom: 1px solid var(--line); }
.brand { display: flex; align-items: center; gap: 9px; font-weight: 650; letter-spacing: 0.02em; }
.dot { width: 10px; height: 10px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 14px rgba(125, 211, 168, 0.8); }
.status { color: var(--muted); font-size: 12px; }
header .spacer { flex: 1; }
.button { color: var(--text); border: 1px solid var(--line); background: var(--panel-2); border-radius: 8px; padding: 6px 11px; font-size: 13px; cursor: pointer; }
.button:hover { border-color: #3b4c5c; }
main { max-width: 980px; margin: 0 auto; padding: 26px 20px 80px; }
.hero { margin: 6vh 0 22px; }
h1 { margin: 0 0 7px; font-size: clamp(26px, 4vw, 44px); letter-spacing: -0.03em; }
.hero p { color: var(--muted); margin: 0; }
.controls { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 12px; }
input { flex: 1 1 340px; min-width: 0; border: 1px solid var(--line); background: var(--panel); color: var(--text); border-radius: 11px; padding: 13px 15px; font: inherit; outline: none; }
select { border: 1px solid var(--line); background: var(--panel); color: var(--text); border-radius: 11px; padding: 0 12px; font: inherit; }
input:focus, select:focus { border-color: var(--accent-2); box-shadow: 0 0 0 3px rgba(91, 184, 255, 0.12); }
.chips { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 18px; }
.chip { font-size: 11px; color: var(--muted); border: 1px solid var(--line); border-radius: 999px; padding: 4px 9px; cursor: pointer; user-select: none; }
.chip input { display: none; }
.chip.on { color: #07110c; background: var(--accent); border-color: var(--accent); }
.count { color: var(--muted); font-size: 12px; margin: 8px 0 12px; }
.results { display: flex; flex-direction: column; gap: 9px; }
.hit { border: 1px solid var(--line); background: var(--panel); border-radius: 12px; padding: 14px 16px; }
.hit:hover { border-color: #3b4c5c; background: var(--panel-2); }
.hit-head { display: flex; align-items: baseline; gap: 9px; }
.kind { flex: 0 0 auto; font-size: 10px; text-transform: uppercase; letter-spacing: 0.08em; color: var(--accent); border: 1px solid rgba(125, 211, 168, 0.25); border-radius: 999px; padding: 1px 7px; }
.kind.peer { color: var(--accent-2); border-color: rgba(91, 184, 255, 0.3); }
.kind.service { color: #f0c674; border-color: rgba(240, 198, 116, 0.3); }
.kind.spore { color: #d8a8ff; border-color: rgba(216, 168, 255, 0.3); }
.title { font-weight: 600; overflow-wrap: anywhere; }
.sub { color: var(--muted); font-size: 12px; margin-top: 3px; }
.refs { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 9px; }
.ref { display: inline-flex; align-items: center; gap: 4px; padding: 3px 7px; border: 1px solid var(--line); border-radius: 999px; color: var(--muted); font-size: 12px; }
.ref b { color: var(--accent-2); font-weight: 600; }
.ref i { color: var(--muted); font-style: normal; }
.snip { color: #b9c7d1; font-size: 13px; margin-top: 8px; overflow-wrap: anywhere; }
.meta { color: var(--muted); font-size: 11px; margin-top: 9px; display: flex; gap: 12px; flex-wrap: wrap; }
.empty { color: var(--muted); text-align: center; margin: 11vh 0; }
</style>
</head>
<body>
<header>
  <div class="brand"><span class="dot"></span><span>Soil Search</span></div>
  <div class="status" id="status">Opt-in discovery</div>
  <div class="spacer"></div>
  <button class="button" id="refresh">Refresh sources</button>
</header>
<main>
  <div class="hero">
    <h1>Find anything you granted.</h1>
    <p>Files, records, repositories, Spores, peers, services, and actions. Search never adds a source on its own.</p>
  </div>
  <div class="controls">
    <input id="q" type="search" placeholder="Search granted sources…" autocomplete="off" spellcheck="false">
    <select id="mode">
      <option value="">auto</option>
      <option value="substring">substring</option>
      <option value="fuzzy">fuzzy</option>
      <option value="full-text">full-text</option>
      <option value="semantic">semantic</option>
      <option value="regex">regex</option>
    </select>
  </div>
  <div class="chips" id="chips"></div>
  <div class="count" id="count"></div>
  <div class="results" id="results"></div>
</main>
<script>
const KINDS = ["file", "record-entry", "repository", "spore", "peer", "service", "action"];
const state = { kinds: new Set(KINDS), hits: [], sources: 0 };
const $ = (id) => document.getElementById(id);

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function renderChips() {
  const chips = $("chips");
  chips.replaceChildren();
  for (const kind of KINDS) {
    const chip = el("label", "chip" + (state.kinds.has(kind) ? " on" : ""));
    const input = el("input");
    input.type = "checkbox";
    input.checked = state.kinds.has(kind);
    input.addEventListener("change", () => {
      if (input.checked) state.kinds.add(kind); else state.kinds.delete(kind);
      renderChips();
      runSearch();
    });
    chip.append(input, el("span", null, kind));
    chips.append(chip);
  }
}

function renderHits() {
  const results = $("results");
  results.replaceChildren();
  if (!state.hits.length) {
    results.append(el("div", "empty", "No results"));
    return;
  }
  for (const hit of state.hits) {
    const card = el("div", "hit");
    const head = el("div", "hit-head");
    head.append(el("span", "kind " + hit.kind, hit.kind), el("span", "title", hit.title));
    card.append(head);
    if (hit.subtitle) card.append(el("div", "sub", hit.subtitle));
    if (hit.snippet) card.append(el("div", "snip", hit.snippet));
    if (hit.references && hit.references.length) {
      const refs = el("div", "refs");
      for (const reference of hit.references) {
        const chip = el("span", "ref");
        chip.append(el("b", null, reference.kind));
        if (reference.rel) chip.append(el("i", null, " " + reference.rel + " "));
        chip.append(el("span", null, reference.title || reference.id));
        chip.title = reference.id;
        refs.append(chip);
      }
      card.append(refs);
    }
    const meta = el("div", "meta");
    meta.append(el("span", null, hit.source), el("span", null, hit.id));
    if (hit.updated_at_ms) meta.append(el("span", null, new Date(hit.updated_at_ms).toLocaleString()));
    card.append(meta);
    results.append(card);
  }
}

async function runSearch() {
  const query = $("q").value.trim();
  if (!query) {
    state.hits = [];
    $("count").textContent = state.sources + " granted sources";
    renderHits();
    return;
  }
  const kinds = KINDS.filter((kind) => state.kinds.has(kind)).join(",");
  try {
    const response = await fetch("/api/search?q=" + encodeURIComponent(query) +
      "&kinds=" + encodeURIComponent(kinds) +
      "&mode=" + encodeURIComponent($("mode").value), { cache: "no-store" });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error || "HTTP " + response.status);
    state.hits = data.hits || [];
    state.sources = data.sources || 0;
    $("count").textContent = state.hits.length + " hits · " + state.sources + " granted sources";
    renderHits();
  } catch (error) {
    state.hits = [];
    $("count").textContent = "Search failed";
    $("results").replaceChildren(el("div", "empty", String(error)));
  }
}

let timer;
$("q").addEventListener("input", () => {
  clearTimeout(timer);
  timer = setTimeout(runSearch, 160);
});
$("mode").addEventListener("change", runSearch);
$("refresh").addEventListener("click", async () => {
  const response = await fetch("/api/refresh", { cache: "no-store" });
  const data = await response.json();
  if (response.ok) {
    state.sources = data.sources || state.sources;
    $("status").textContent = "generation " + data.generation;
    runSearch();
  }
});
renderChips();
$("count").textContent = "Opt-in discovery";
</script>
</body>
</html>
"#;
