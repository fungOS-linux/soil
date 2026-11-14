//! Read-only discovery UI for `soil:search`.
//!
//! Sources are opt-in host arguments: VFS, records books, Git roots, Spore
//! roots, and Mycelium peer/service snapshots. The browser can only ask for
//! kinds the host already configured.

use std::{
    collections::HashMap,
    env,
    io::{self, BufRead, BufReader, Read, Write},
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
use soil_search_host::{LauncherConfig, SearchService, api, applications};
use soil::vfs_host::VfsHost;

const DEFAULT_LISTEN: &str = "127.0.0.1:8790";
const MAX_PAGE: u32 = 128;
struct App {
    service: Mutex<SearchService>,
    sources: u32,
    /// Bundles discovered on this machine. Empty when the host was handed a
    /// snapshot instead, or told not to look.
    installed: Vec<applications::InstalledApp>,
    by_id: HashMap<String, usize>,
    icons: Mutex<HashMap<String, Option<Arc<Vec<u8>>>>>,
}

impl App {
    /// A bundle's icon, decoded once. `None` is remembered too: a bundle without
    /// extractable artwork must not be re-read on every keystroke.
    fn icon(&self, bundle_id: &str) -> Option<Arc<Vec<u8>>> {
        let index = *self.by_id.get(bundle_id)?;
        let mut icons = self.icons.lock().expect("icon cache poisoned");
        if let Some(cached) = icons.get(bundle_id) {
            return cached.clone();
        }
        let bytes = applications::icon_png(&self.installed[index]).map(Arc::new);
        icons.insert(bundle_id.to_owned(), bytes.clone());
        bytes
    }

    /// What the row draws: the app's own artwork when the bundle carries some,
    /// generated art otherwise. A locator travels, never the bytes.
    fn art(&self, hit: &api::Hit) -> Value {
        let key = percent_encode(&hit.id);
        if hit.kind == api::Kind::Application && self.icon(&hit.id).is_some() {
            return json!({"png": format!("/icon/{key}.png")});
        }
        json!({
            "svg": format!(
                "/icon/{key}.svg?kind={}&title={}",
                percent_encode(kind_name(hit.kind)),
                percent_encode(&hit.title)
            )
        })
    }
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
    no_applications: bool,
    mru: Option<PathBuf>,
    config: Option<PathBuf>,
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
    if let Some(path) = &args.mru {
        service = service.with_mru(path);
    }
    if let Some(path) = &args.config {
        service = service.with_launcher_config(path);
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
    // Applications come from the machine unless the host was handed an explicit
    // inventory: a launcher that ships a list is a launcher that lies.
    let installed = if args.no_applications || args.applications_snapshot.is_some() {
        Vec::new()
    } else {
        let scanned = applications::scan(&applications::bundle_roots());
        service = service.with_installed_applications(&scanned);
        scanned
    };
    let by_id = installed
        .iter()
        .enumerate()
        .map(|(index, app)| (app.bundle_id.clone(), index))
        .collect();
    let application_count = installed.len();
    let app = Arc::new(App {
        service: Mutex::new(service),
        sources,
        installed,
        by_id,
        icons: Mutex::new(HashMap::new()),
    });
    let listen = args.listen.unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    let listener = TcpListener::bind(&listen).with_context(|| format!("bind {listen}"))?;
    println!("soil search: http://{listen}/  sources={sources} applications={application_count}");
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
    let Some((method, target, body)) = read_request(&mut stream)? else {
        return Ok(());
    };
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target.as_str(), ""),
    };
    if path == "/api/config" && method == "POST" {
        return write_config(&mut stream, app, &body);
    }
    if path == "/api/use" {
        if method != "POST" {
            return respond(
                &mut stream,
                "405 Method Not Allowed",
                "text/plain",
                b"use records a launch; POST one\n",
            );
        }
        return record_use(&mut stream, app, &body);
    }
    if let Some(name) = path.strip_prefix("/icon/") {
        return icon_route(&mut stream, app, name, query);
    }
    if method != "GET" && method != "HEAD" {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed\n",
        );
    }
    match path {
        "/" => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            LAUNCHER_HTML.as_bytes(),
        ),
        // The bare discovery view stays reachable for inspecting what the host
        // actually matched; the launcher is what a person runs.
        "/search" | "/index.html" => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        "/health" => respond(&mut stream, "200 OK", "text/plain; charset=utf-8", b"ok\n"),
        "/api/tiles" => {
            let limit = query_param(query, "limit")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(24)
                .clamp(1, 128);
            let (hits, config) = {
                let service = app.service.lock().expect("search service poisoned");
                let hits = service.tiles(limit);
                let config = service.launcher_config().clone();
                (hits, config)
            };
            respond_json(
                &mut stream,
                "200 OK",
                &json!({
                    "tiles": hits_json(app, hits, &config),
                    "profile": config.profile,
                    "sources": app.sources,
                }),
            )
        }
        "/api/config" => {
            let config = {
                let service = app.service.lock().expect("search service poisoned");
                service.launcher_config().clone()
            };
            respond_json(
                &mut stream,
                "200 OK",
                &json!({
                    "profile": config.profile,
                    "pins": config.pins,
                    "hidden_kinds": config.hidden_kinds,
                }),
            )
        }
        "/api/search" => {
            let text = query_param(query, "q").unwrap_or_default();
            let limit = query_param(query, "limit")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(MAX_PAGE)
                .clamp(1, MAX_PAGE);
            if text.trim().is_empty() {
                // At rest the launcher is a wall, not a list: pins first, then
                // what this device opens, answered without a scan.
                let (hits, config, generation) = {
                    let mut service = app.service.lock().expect("search service poisoned");
                    let hits = service.tiles(limit);
                    let config = service.launcher_config().clone();
                    let generation = service.state().map(|state| state.generation).unwrap_or(0);
                    (hits, config, generation)
                };
                return respond_json(
                    &mut stream,
                    "200 OK",
                    &json!({
                        "query": "",
                        "hits": hits_json(app, hits, &config),
                        "next": null,
                        "rest": true,
                        "generation": generation,
                        "sources": app.sources,
                        "profile": config.profile,
                    }),
                );
            }
            let kinds = parse_kinds(query_param(query, "kinds").unwrap_or_default());
            let mode = parse_mode(query_param(query, "mode").unwrap_or_default());
            let offset = query_param(query, "offset")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let result = {
                let mut service = app.service.lock().expect("search service poisoned");
                let page = service.search(api::Query {
                    text,
                    kinds,
                    mode,
                    offset,
                    limit,
                });
                let config = service.launcher_config().clone();
                page.map(|page| (page, config))
            };
            match result {
                Ok((page, config)) => respond_json(
                    &mut stream,
                    "200 OK",
                    &json!({
                        "hits": hits_json(app, page.hits, &config),
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

/// Render hits with the two things only the host knows: whether a row is pinned
/// and, for a Spore, which surfaces it would open. Called after the service lock
/// is released; it takes its own for the surface lookup.
fn hits_json(app: &App, hits: Vec<api::Hit>, config: &LauncherConfig) -> Vec<Value> {
    let surfaces: HashMap<String, Vec<String>> = {
        let service = app.service.lock().expect("search service poisoned");
        hits.iter()
            .filter(|hit| hit.kind == api::Kind::Spore)
            .filter_map(|hit| {
                service
                    .spore_surfaces(&hit.id)
                    .map(|list| (hit.id.clone(), list))
            })
            .collect()
    };
    hits.into_iter()
        .map(|hit| {
            let pinned = config.pinned_rank(&hit.id).is_some();
            let surfaces = surfaces.get(&hit.id).map(Vec::as_slice);
            hit_json(app, hit, pinned, surfaces)
        })
        .collect()
}

fn write_config(stream: &mut TcpStream, app: &App, body: &[u8]) -> Result<()> {
    let Ok(config) = serde_json::from_slice::<LauncherConfig>(body) else {
        return respond_json(
            stream,
            "400 Bad Request",
            &json!({"error": "config needs profile, pins and hidden_kinds"}),
        );
    };
    let result = {
        let mut service = app.service.lock().expect("search service poisoned");
        service.set_launcher_config(config)
    };
    match result {
        Ok(config) => respond_json(
            stream,
            "200 OK",
            &json!({
                "ok": true,
                "profile": config.profile,
                "pins": config.pins,
                "hidden_kinds": config.hidden_kinds,
            }),
        ),
        Err(error) => respond_json(
            stream,
            "500 Internal Server Error",
            &json!({"error": error.to_string()}),
        ),
    }
}

fn hit_json(app: &App, hit: api::Hit, pinned: bool, surfaces: Option<&[String]>) -> Value {
    let art = app.art(&hit);
    json!({
        "pinned": pinned,
        "surfaces": surfaces,
        "kind": kind_name(hit.kind),
        "id": hit.id,
        "title": hit.title,
        "subtitle": hit.subtitle,
        "snippet": hit.snippet,
        "score": hit.score,
        "source": hit.source,
        "updated_at_ms": hit.updated_at_ms,
        "icon": hit.icon,
        "art": art,
        "references": hit.references.into_iter().map(|reference| json!({
            "kind": reference.kind,
            "id": reference.id,
            "rel": reference.rel,
            "title": reference.title,
        })).collect::<Vec<_>>(),
    })
}

/// Record a launch. The caller sends the row it launched; the host owns whether
/// that becomes memory, so a launcher cannot ask for a row to become frequent.
fn record_use(stream: &mut TcpStream, app: &App, body: &[u8]) -> Result<()> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return respond_json(
            stream,
            "400 Bad Request",
            &json!({"error": "use needs a JSON row"}),
        );
    };
    let Some(id) = value.get("id").and_then(Value::as_str) else {
        return respond_json(
            stream,
            "400 Bad Request",
            &json!({"error": "use needs an id"}),
        );
    };
    let hit = api::Hit {
        kind: value
            .get("kind")
            .and_then(Value::as_str)
            .and_then(kind_from_name)
            .unwrap_or(api::Kind::File),
        id: id.to_owned(),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_owned(),
        subtitle: value
            .get("subtitle")
            .and_then(Value::as_str)
            .map(str::to_owned),
        snippet: None,
        score: 0,
        source: value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("host")
            .to_owned(),
        updated_at_ms: None,
        references: Vec::new(),
        icon: None,
    };
    let result = {
        let mut service = app.service.lock().expect("search service poisoned");
        service.record_use(&hit)
    };
    match result {
        Ok(()) => respond_json(stream, "200 OK", &json!({"ok": true})),
        Err(error) => respond_json(
            stream,
            "500 Internal Server Error",
            &json!({"error": error.to_string()}),
        ),
    }
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
    soil_search_host::kind_name(kind)
}

fn parse_kinds(value: String) -> Vec<api::Kind> {
    value.split(',').filter_map(kind_from_name).collect()
}

fn kind_from_name(name: &str) -> Option<api::Kind> {
    soil_search_host::kind_from_name(name)
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

fn read_request(stream: &mut TcpStream) -> io::Result<Option<(String, String, Vec<u8>)>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    if reader.read_line(&mut request)? == 0 {
        return Ok(None);
    }
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    // Bounded on purpose: a request body here is one launcher row.
    let mut body = vec![0u8; content_length.min(64 * 1024)];
    if !body.is_empty() {
        reader.read_exact(&mut body)?;
    }
    Ok(Some((method, target, body)))
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

/// `/icon/<encoded id>.png` serves the bundle's own artwork; `.svg` serves art
/// generated from the row, so a hit without an icon is a designed tile rather
/// than a hole.
fn icon_route(stream: &mut TcpStream, app: &App, name: &str, query: &str) -> Result<()> {
    let name = percent_decode(name);
    let Some((id, extension)) = name.rsplit_once('.') else {
        return respond(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"no artwork\n",
        );
    };
    match extension {
        "png" => match app.icon(id) {
            Some(bytes) => respond(stream, "200 OK", "image/png", bytes.as_slice()),
            None => respond(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"no artwork\n",
            ),
        },
        "svg" => {
            let kind = query_param(query, "kind").unwrap_or_else(|| "file".into());
            let title = query_param(query, "title").unwrap_or_else(|| id.to_owned());
            respond(
                stream,
                "200 OK",
                "image/svg+xml; charset=utf-8",
                art_svg(&kind, id, &title).as_bytes(),
            )
        }
        _ => respond(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"no artwork\n",
        ),
    }
}

// ---- generated artwork -----------------------------------------------------
//
// An app has a brand and takes its own artwork; every other kind is a category
// and should look like the category. Applications hash their id to a hue, files
// read their extension, and the rest key off the kind, so a row is greppable by
// shape without reading it.

const ART_FONT: &str = "-apple-system, BlinkMacSystemFont, 'SF Pro Text', system-ui, sans-serif";

fn fnv(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn art_hue(key: &str) -> u32 {
    (fnv(key) % 360) as u32
}

fn art_kind_hue(kind: &str) -> Option<u32> {
    match kind {
        "action" => Some(18),
        "spore" => Some(288),
        "principal" => Some(198),
        "repository" => Some(138),
        "service" => Some(222),
        "task" => Some(152),
        "record-entry" => Some(264),
        "peer" => Some(6),
        _ => None,
    }
}

fn art_initials(title: &str) -> String {
    let words: Vec<&str> = title
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .collect();
    match words.as_slice() {
        [] => "?".to_string(),
        [only] => only
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string(),
        [first, second, ..] => {
            let head = first.chars().next().unwrap_or('?');
            let tail = second.chars().next().unwrap_or('?');
            format!("{head}{tail}").to_uppercase()
        }
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn art_defs(hue: u32) -> String {
    format!(
        "<defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\".35\" y2=\"1\">\
         <stop offset=\"0\" stop-color=\"hsl({hue}, 92%, 68%)\"/>\
         <stop offset=\"1\" stop-color=\"hsl({}, 78%, 42%)\"/></linearGradient>\
         <linearGradient id=\"gloss\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\
         <stop offset=\"0\" stop-color=\"#fff\" stop-opacity=\".42\"/>\
         <stop offset=\"1\" stop-color=\"#fff\" stop-opacity=\"0\"/></linearGradient></defs>",
        (hue + 28) % 360
    )
}

fn art_squircle() -> &'static str {
    "<rect x=\"4\" y=\"4\" width=\"120\" height=\"120\" rx=\"30\" fill=\"url(#g)\"/>\
     <path d=\"M4 34a30 30 0 0 1 30-30h60a30 30 0 0 1 30 30v26H4z\" fill=\"url(#gloss)\"/>\
     <rect x=\"4.5\" y=\"4.5\" width=\"119\" height=\"119\" rx=\"29.5\" fill=\"none\" stroke=\"#fff\" stroke-opacity=\".22\"/>"
}

fn art_glyph(kind: &str, initials: &str) -> String {
    match kind {
        "principal" => "<circle cx=\"64\" cy=\"52\" r=\"18\" fill=\"#fff\" fill-opacity=\".94\"/>\
            <path d=\"M30 104a34 30 0 0 1 68 0z\" fill=\"#fff\" fill-opacity=\".94\"/>"
            .to_string(),
        "action" => {
            "<path d=\"M72 16 38 72h22l-6 40 38-58H70z\" fill=\"#fff\" fill-opacity=\".94\"/>"
                .to_string()
        }
        "spore" => "<circle cx=\"64\" cy=\"64\" r=\"15\" fill=\"#fff\" fill-opacity=\".94\"/>\
            <circle cx=\"64\" cy=\"26\" r=\"7\" fill=\"#fff\" fill-opacity=\".62\"/>\
            <circle cx=\"95\" cy=\"82\" r=\"7\" fill=\"#fff\" fill-opacity=\".62\"/>\
            <circle cx=\"33\" cy=\"82\" r=\"7\" fill=\"#fff\" fill-opacity=\".62\"/>"
            .to_string(),
        "repository" => "<path d=\"M46 38v54\" stroke=\"#fff\" stroke-opacity=\".94\" stroke-width=\"8\" stroke-linecap=\"round\"/>\
            <path d=\"M46 92c0-26 14-38 38-38\" stroke=\"#fff\" stroke-opacity=\".94\" stroke-width=\"8\" stroke-linecap=\"round\" fill=\"none\"/>\
            <circle cx=\"46\" cy=\"34\" r=\"10\" fill=\"#fff\" fill-opacity=\".94\"/>\
            <circle cx=\"46\" cy=\"96\" r=\"10\" fill=\"#fff\" fill-opacity=\".94\"/>\
            <circle cx=\"88\" cy=\"54\" r=\"10\" fill=\"#fff\" fill-opacity=\".94\"/>"
            .to_string(),
        "service" => "<path d=\"M38 96a34 34 0 0 1 34-34\" stroke=\"#fff\" stroke-opacity=\".5\" stroke-width=\"8\" stroke-linecap=\"round\" fill=\"none\"/>\
            <path d=\"M38 96a58 58 0 0 1 58-58\" stroke=\"#fff\" stroke-opacity=\".8\" stroke-width=\"8\" stroke-linecap=\"round\" fill=\"none\"/>\
            <circle cx=\"38\" cy=\"96\" r=\"10\" fill=\"#fff\" fill-opacity=\".94\"/>"
            .to_string(),
        "task" => "<path d=\"M34 66l20 20 40-46\" stroke=\"#fff\" stroke-opacity=\".95\" stroke-width=\"13\" stroke-linecap=\"round\" stroke-linejoin=\"round\" fill=\"none\"/>"
            .to_string(),
        "record-entry" => "<rect x=\"28\" y=\"46\" width=\"72\" height=\"60\" rx=\"10\" fill=\"#fff\" fill-opacity=\".38\"/>\
            <rect x=\"36\" y=\"30\" width=\"72\" height=\"60\" rx=\"10\" fill=\"#fff\" fill-opacity=\".94\"/>\
            <rect x=\"48\" y=\"46\" width=\"40\" height=\"7\" rx=\"3.5\" fill=\"#0b0e14\" fill-opacity=\".35\"/>\
            <rect x=\"48\" y=\"60\" width=\"30\" height=\"7\" rx=\"3.5\" fill=\"#0b0e14\" fill-opacity=\".22\"/>"
            .to_string(),
        "peer" => "<circle cx=\"42\" cy=\"64\" r=\"16\" fill=\"#fff\" fill-opacity=\".94\"/>\
            <circle cx=\"88\" cy=\"64\" r=\"16\" fill=\"#fff\" fill-opacity=\".94\"/>\
            <path d=\"M58 64h14\" stroke=\"#fff\" stroke-opacity=\".9\" stroke-width=\"8\" stroke-linecap=\"round\"/>"
            .to_string(),
        _ => format!(
            "<text x=\"64\" y=\"64\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{ART_FONT}\" font-size=\"54\" font-weight=\"700\" fill=\"#fff\" fill-opacity=\".96\">{}</text>",
            escape_xml(initials)
        ),
    }
}

fn art_file(label: &str) -> String {
    format!(
        "<path d=\"M28 6h46l26 26v90a6 6 0 0 1-6 6H28a6 6 0 0 1-6-6V12a6 6 0 0 1 6-6z\" fill=\"url(#g)\"/>\
         <path d=\"M74 6l26 26H76a2 2 0 0 1-2-2z\" fill=\"#fff\" fill-opacity=\".55\"/>\
         <rect x=\"30\" y=\"74\" width=\"60\" height=\"30\" rx=\"7\" fill=\"#0b0e14\" fill-opacity=\".42\"/>\
         <text x=\"60\" y=\"89\" text-anchor=\"middle\" dominant-baseline=\"central\" font-family=\"{ART_FONT}\" font-size=\"17\" font-weight=\"700\" letter-spacing=\"1\" fill=\"#fff\" fill-opacity=\".95\">{}</text>",
        escape_xml(label)
    )
}

fn art_file_style(extension: &str) -> (u32, String) {
    let hue = match extension.to_lowercase().as_str() {
        "md" => 210,
        "rs" => 22,
        "wit" => 282,
        "lua" => 224,
        "toml" => 38,
        "json" => 46,
        "py" => 204,
        "ts" => 190,
        "png" => 300,
        "pdf" => 4,
        "txt" => 205,
        "sh" => 130,
        _ => 212,
    };
    let label = if extension.is_empty() {
        "FILE".to_string()
    } else {
        extension.chars().take(3).collect::<String>().to_uppercase()
    };
    (hue, label)
}

/// Deterministic, colourful artwork for a hit that ships no icon of its own.
fn art_svg(kind: &str, id: &str, title: &str) -> String {
    let kind = kind.trim().to_lowercase();
    let body = if kind == "file" {
        let extension = std::path::Path::new(title)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();
        let (hue, label) = art_file_style(extension);
        format!("{}{}", art_defs(hue), art_file(&label))
    } else {
        let hue = art_kind_hue(&kind).unwrap_or_else(|| art_hue(id));
        format!(
            "{}{}{}",
            art_defs(hue),
            art_squircle(),
            art_glyph(&kind, &art_initials(title))
        )
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 128 128\" width=\"128\" height=\"128\">{body}</svg>"
    )
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
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
                    iter.next()
                        .context("--applications-snapshot needs a value")?,
                ));
            }
            "--no-applications" => args.no_applications = true,
            "--config" => {
                args.config = Some(PathBuf::from(
                    iter.next().context("--config needs a value")?,
                ));
            }
            "--marbles-snapshot" => {
                args.marbles_snapshot = Some(PathBuf::from(
                    iter.next().context("--marbles-snapshot needs a value")?,
                ));
            }
            "--mru" => {
                args.mru = Some(PathBuf::from(iter.next().context("--mru needs a value")?));
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
                    "soil-search-web [--listen ADDR] [--vfs-root DIR] [--records-root DIR] [--book BOOK]...\n\
                     \t[--git-root DIR]... [--spore-root DIR]... [--mycelium-snapshot DIR]\n\
                     \t[--applications-snapshot FILE] [--no-applications]\n\
                     \t[--mru FILE] [--config FILE]\n\
                     default: http://{DEFAULT_LISTEN}/"
                );
                return Ok(None);
            }
            other => anyhow::bail!("unknown argument {other}; try --help"),
        }
    }
    Ok(Some(args))
}

const LAUNCHER_HTML: &str = include_str!("../launcher.html");

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
