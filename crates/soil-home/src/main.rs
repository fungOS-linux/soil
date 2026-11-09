//! `soil-home`: the front door for the names this machine serves.
//!
//! The Mycelium gateway owns DNS, TLS and routing; this only answers `home.myc`
//! with the routes in `~/.mycelium/gateway/routes.json`, drawn as a launcher:
//! wallpaper, a status bar, widgets, and a grid of app tiles. It reads the route
//! file per request, so a new route appears without restarting anything, and it
//! reads the store's catalog for each Spore's mark (badge and colour) and the
//! pending-update count.
//!
//! The route file decides the order. Nothing here curates names or descriptions;
//! an unknown surface gets a colour hashed from its name and its first letter.
//!
//! The palette matches `soil-store`'s presenter on purpose: these are two pages
//! of one front door, not two products.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_ADDR: &str = "127.0.0.1:8801";
const STORE_ADDR: &str = "127.0.0.1:8800";
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const STORE_TIMEOUT: Duration = Duration::from_millis(350);
const STORE_TTL: Duration = Duration::from_secs(3);

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-home: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("soil-home {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", usage());
        return Ok(());
    }

    let routes = named(&args, "--routes").map_or_else(default_routes, PathBuf::from);
    let wallpaper = named(&args, "--wallpaper")
        .map(PathBuf::from)
        .or_else(default_wallpaper)
        .filter(|path| path.is_file());
    let address = named(&args, "--addr").unwrap_or_else(|| DEFAULT_ADDR.to_owned());
    let listener =
        TcpListener::bind(&address).map_err(|error| format!("listen {address}: {error}"))?;
    match listener.local_addr() {
        Ok(bound) => eprintln!("soil-home: http://{bound}/ reading {}", routes.display()),
        Err(_) => eprintln!("soil-home: http://{address}/ reading {}", routes.display()),
    }
    if let Some(wallpaper) = &wallpaper {
        eprintln!("soil-home: wallpaper {}", wallpaper.display());
    }

    let cache = Arc::new(Mutex::new(Cache::default()));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let routes = routes.clone();
        let cache = Arc::clone(&cache);
        let wallpaper = wallpaper.clone();
        std::thread::spawn(move || {
            let _ = answer(stream, &routes, &cache, wallpaper.as_deref());
        });
    }
    Ok(())
}

fn default_routes() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".mycelium/gateway/routes.json")
}

/// A wallpaper is a file the person chose, not something this page ships.
fn default_wallpaper() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".local/share/fungos/home/wallpaper"))
}

fn named(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn usage() -> String {
    "usage: soil-home [--routes PATH] [--wallpaper PATH] [--addr HOST:PORT]\n       soil-home --version".into()
}

// ---- the route table ------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize)]
struct Gateway {
    #[serde(default)]
    routes: Vec<Route>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Route {
    name: String,
    #[serde(default)]
    hosts: Vec<String>,
    #[serde(default)]
    upstream: Option<Upstream>,
    #[serde(default)]
    service_kind: Option<String>,
    /// The Spore whose mark this surface should wear, when the operator names
    /// one. The gateway ignores the field; it is read here and by nothing else.
    #[serde(default)]
    spore: Option<String>,
}

/// `upstream` is a string in most routes and a list in the ones with fallbacks.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum Upstream {
    One(String),
    Many(Vec<String>),
}

impl Upstream {
    fn first(&self) -> Option<&str> {
        match self {
            Self::One(url) => Some(url),
            Self::Many(urls) => urls.first().map(String::as_str),
        }
    }
}

impl Route {
    /// The name a browser should visit: the first `.myc` host, else the first.
    fn host(&self) -> Option<&str> {
        self.hosts
            .iter()
            .find(|host| host.ends_with(".myc"))
            .or_else(|| self.hosts.first())
            .map(String::as_str)
    }
}

fn read_routes(path: &Path) -> Result<Vec<Route>, String> {
    let source =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let gateway: Gateway =
        serde_json::from_str(&source).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut routes = gateway.routes;
    routes.retain(|route| route.host().is_some());
    Ok(routes)
}

// ---- the store's view of the Spores ---------------------------------------

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct App {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    badge: Option<String>,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct Pending {
    #[serde(default)]
    total: u64,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct StoreState {
    #[serde(default)]
    apps: Vec<App>,
    #[serde(default)]
    pending: Pending,
}

#[derive(Default)]
struct Cache {
    at: Option<Instant>,
    state: StoreState,
}

/// The store's snapshot, cached briefly: a page load should not wait on a
/// service that is not answering, and three seconds of staleness is invisible.
fn store_state(cache: &Mutex<Cache>) -> StoreState {
    let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(at) = cache.at
        && at.elapsed() < STORE_TTL
    {
        return cache.state.clone();
    }
    let state = fetch_store();
    cache.at = Some(Instant::now());
    cache.state = state.clone();
    state
}

fn fetch_store() -> StoreState {
    let Ok(body) = get(STORE_ADDR, "/api/apps") else {
        return StoreState::default();
    };
    serde_json::from_str(&body).unwrap_or_default()
}

/// One loopback GET. The store is a local presenter, so this is a socket read,
/// not an HTTP client: no TLS, no redirects, no pooling.
fn get(address: &str, path: &str) -> Result<String, String> {
    let socket: SocketAddr = address
        .parse()
        .map_err(|error| format!("{address}: {error}"))?;
    let mut stream = TcpStream::connect_timeout(&socket, STORE_TIMEOUT)
        .map_err(|error| format!("{address}: {error}"))?;
    stream
        .set_read_timeout(Some(STORE_TIMEOUT))
        .map_err(|error| format!("{address}: {error}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| format!("{address}: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("{address}: {error}"))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("{address}: {error}"))?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("{address}: response has no body"))?;
    if !head.starts_with("HTTP/1.1 200") {
        return Err(format!("{address}: {}", head.lines().next().unwrap_or("")));
    }
    Ok(body.to_owned())
}

/// Whether the route's first upstream accepts a connection right now.
fn probe(route: &Route) -> &'static str {
    let Some(url) = route.upstream.as_ref().and_then(Upstream::first) else {
        return if route.service_kind.is_some() {
            "gateway"
        } else {
            "unknown"
        };
    };
    let Some(host) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return "unknown";
    };
    let host = host.split('/').next().unwrap_or(host);
    match host.parse() {
        Ok(address) => match TcpStream::connect_timeout(&address, PROBE_TIMEOUT) {
            Ok(_) => "up",
            Err(_) => "down",
        },
        Err(_) => "unknown",
    }
}

/// One probe per route, in parallel, so a stopped upstream costs one timeout and
/// not one per card.
fn statuses(routes: &[Route]) -> Vec<&'static str> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = routes
            .iter()
            .map(|route| scope.spawn(move || probe(route)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or("unknown"))
            .collect()
    })
}

// ---- pages ----------------------------------------------------------------

fn answer(
    mut stream: TcpStream,
    routes_path: &Path,
    cache: &Mutex<Cache>,
    wallpaper: Option<&Path>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or(target);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let (status, content_type, body) = match (method, path) {
        ("GET", "/healthz") => ("200 OK", TEXT, b"ok\n".to_vec()),
        ("GET", "/api/routes") => match read_routes(routes_path) {
            Ok(routes) => ("200 OK", JSON, routes_json(&routes).into_bytes()),
            Err(error) => (
                "503 Service Unavailable",
                TEXT,
                format!("{error}\n").into_bytes(),
            ),
        },
        ("GET", "/") => match read_routes(routes_path) {
            Ok(routes) => {
                let store = store_state(cache);
                (
                    "200 OK",
                    HTML,
                    page(&routes, &store, wallpaper).into_bytes(),
                )
            }
            Err(error) => (
                "503 Service Unavailable",
                HTML,
                document(
                    &format!("<main><p class=\"error\">{}</p></main>", escape(&error)),
                    false,
                )
                .into_bytes(),
            ),
        },
        ("GET", "/wallpaper") => match wallpaper {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => ("200 OK", wallpaper_type(path), bytes),
                Err(_) => ("404 Not Found", TEXT, "no wallpaper\n".as_bytes().to_vec()),
            },
            None => ("404 Not Found", TEXT, "no wallpaper\n".as_bytes().to_vec()),
        },
        _ => ("404 Not Found", TEXT, b"not found\n".to_vec()),
    };

    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()
}

fn wallpaper_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn routes_json(routes: &[Route]) -> String {
    let rows: Vec<serde_json::Value> = routes
        .iter()
        .map(|route| {
            serde_json::json!({
                "name": route.name,
                "hosts": route.hosts,
                "upstream": route.upstream.as_ref().and_then(Upstream::first),
                "service_kind": route.service_kind,
                "status": probe(route),
            })
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

/// The Spore whose mark this route wears, if any.
fn mark<'a>(route: &Route, apps: &'a [App]) -> Option<&'a App> {
    if let Some(spore) = &route.spore {
        return apps.iter().find(|app| &app.id == spore);
    }
    apps.iter().find(|app| {
        let short = app.id.rsplit('.').next().unwrap_or(&app.id);
        short == route.name || short.starts_with(&route.name) || route.name.starts_with(short)
    })
}

/// A stable colour for a surface with no Spore: same name, same hue, forever.
fn hashed_color(name: &str) -> String {
    let mut hash: u32 = 2166136261;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16777619);
    }
    let hue = hash % 360;
    format!("hsl({hue} 58% 55%)")
}

fn page(routes: &[Route], store: &StoreState, wallpaper: Option<&Path>) -> String {
    let statuses = statuses(routes);
    let mut tiles = String::new();
    for (route, status) in routes.iter().zip(statuses) {
        // The launcher is not one of its own apps.
        if route.name == "home" {
            continue;
        }
        let host = route.host().unwrap_or_default();
        let app = mark(route, &store.apps);
        let label = app
            .map(|app| app.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| route.name.clone());
        let badge = app
            .and_then(|app| app.badge.clone())
            .filter(|badge| !badge.is_empty() && badge != "?")
            .unwrap_or_else(|| label.chars().next().map(String::from).unwrap_or_default())
            .to_uppercase();
        let color = app
            .and_then(|app| app.color.clone())
            .unwrap_or_else(|| hashed_color(&route.name));
        let needle = format!("{} {} {}", route.name, label, host).to_lowercase();
        tiles.push_str(&format!(
            r#"<a class="app" href="https://{host}/" data-search="{needle}"><span class="icon" style="--c:{color}"><span class="badge">{badge}</span><span class="dot {status}"></span></span><span class="label">{label}</span></a>"#,
            host = escape(host),
            needle = escape(&needle),
            color = escape(&color),
            badge = escape(&badge),
            status = status,
            label = escape(&label),
        ));
    }
    if tiles.is_empty() {
        tiles.push_str("<p class=\"empty\">No routes are configured yet.</p>");
    }

    let up = routes.iter().filter(|route| probe(route) == "up").count();
    let down = routes.iter().filter(|route| probe(route) == "down").count();
    let pending = if store.apps.is_empty() {
        "<p class=\"muted\">the store is not answering</p>".to_owned()
    } else if store.pending.total == 0 {
        "<p class=\"muted\">every Spore is current</p>".to_owned()
    } else {
        format!(
            "<p><a class=\"link\" href=\"https://store.myc/updates\">{} updates ready</a></p>",
            store.pending.total
        )
    };

    let body = format!(
        r#"<header class="bar"><div class="when"><span id="clock">--:--</span><span id="day">—</span></div><div class="who">fungOS</div><div class="find"><input id="search" type="search" placeholder="Search apps" autocomplete="off"></div></header><main><section class="widgets"><article class="widget"><span class="key">Today</span><p class="big" id="today">—</p><p class="muted" id="today-date"></p></article><article class="widget"><span class="key">This machine</span><p class="big">{up}<span class="muted"> of {count} services up</span></p>{pending}<p class="muted">{down} down</p></article></section><section class="grid" id="grid">{tiles}</section><p class="empty" id="none" hidden>Nothing matches that name.</p></main><script>{script}</script>"#,
        up = up,
        count = routes.len(),
        down = down,
        pending = pending,
        tiles = tiles,
        script = SCRIPT,
    );
    document(&body, wallpaper.is_some())
}

fn document(body: &str, wall: bool) -> String {
    let class = if wall { " class=\"wall\"" } else { "" };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><meta name="theme-color" content='#0b1020'><title>fungOS</title><style>{STYLE}</style></head><body{class}>{body}</body></html>"#
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

const HTML: &str = "text/html; charset=utf-8";
const JSON: &str = "application/json";
const TEXT: &str = "text/plain; charset=utf-8";

/// Filtering happens in the page: the grid is small, and a round trip per
/// keystroke would make search feel slower than the eye. The clock is local
/// too, because the server is a socket, not a time service.
const SCRIPT: &str = r#"
const pad = (n) => String(n).padStart(2, '0');
const update = () => {
  const now = new Date();
  document.getElementById('clock').textContent = `${pad(now.getHours())}:${pad(now.getMinutes())}`;
  document.getElementById('day').textContent = now.toLocaleDateString(undefined, { weekday: 'long', month: 'short', day: 'numeric' });
  document.getElementById('today').textContent = `${pad(now.getHours())}:${pad(now.getMinutes())}`;
  document.getElementById('today-date').textContent = now.toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric' });
};
update();
setInterval(update, 10000);

const search = document.getElementById('search');
const apps = [...document.querySelectorAll('#grid .app')];
const none = document.getElementById('none');
const filter = () => {
  const needle = search.value.trim().toLowerCase();
  let shown = 0;
  for (const app of apps) {
    const match = !needle || app.dataset.search.includes(needle);
    app.hidden = !match;
    if (match) shown += 1;
  }
  none.hidden = shown > 0;
};
search.addEventListener('input', filter);
document.addEventListener('keydown', (event) => {
  if (event.key === '/' && document.activeElement !== search) {
    event.preventDefault();
    search.focus();
  }
});
"#;

const STYLE: &str = r#"
:root { color-scheme: dark; --ink: #f4f6fb; --muted: #a9b1c4; }
* { box-sizing: border-box; }
html, body { min-height: 100%; }
body {
  margin: 0; color: var(--ink); font: 15px/1.4 -apple-system, BlinkMacSystemFont, ui-sans-serif, system-ui, sans-serif;
  background:
    radial-gradient(1100px 760px at 16% 8%, #2b3a67 0%, transparent 62%),
    radial-gradient(900px 680px at 86% 14%, #4b2e63 0%, transparent 58%),
    radial-gradient(1000px 820px at 52% 112%, #16324a 0%, transparent 62%),
    linear-gradient(160deg, #0b1020, #05070d 72%);
  background-attachment: fixed;
  min-height: 100vh; display: flex; flex-direction: column;
}
body.wall { background: #05070d; }
body.wall::before { content: ""; position: fixed; inset: 0; z-index: -2; background: url("/wallpaper") center / cover no-repeat; }
body.wall::after { content: ""; position: fixed; inset: 0; z-index: -1; background: linear-gradient(rgba(5,7,13,0.45), rgba(5,7,13,0.72)); }
a { color: inherit; text-decoration: none; }
.bar { display: flex; align-items: center; justify-content: space-between; gap: 18px; padding: calc(14px + env(safe-area-inset-top)) 26px 10px; font-size: 13px; color: var(--muted); }
.when { display: flex; gap: 10px; align-items: baseline; }
#clock { font-weight: 600; color: var(--ink); }
.who { font-weight: 600; letter-spacing: 0.14em; text-transform: uppercase; font-size: 11px; }
.find input { width: 240px; padding: 8px 14px; border-radius: 999px; border: 1px solid rgba(255,255,255,0.16); background: rgba(18,22,34,0.55); color: var(--ink); font: inherit; font-size: 13px; outline: none; backdrop-filter: blur(14px); }
.find input:focus { border-color: rgba(255,255,255,0.38); background: rgba(24,30,46,0.72); }
main { flex: 1; padding: 18px 32px 72px; max-width: 1180px; width: 100%; margin: 0 auto; }
.widgets { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 16px; margin-bottom: 34px; }
.widget { padding: 18px 20px; border-radius: 22px; border: 1px solid rgba(255,255,255,0.1); background: rgba(18,22,34,0.5); backdrop-filter: blur(18px); box-shadow: 0 18px 40px rgba(0,0,0,0.28); }
.key { display: block; font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--muted); }
.big { margin: 6px 0 2px; font-size: 30px; font-weight: 600; letter-spacing: -0.02em; }
.muted { margin: 2px 0 0; color: var(--muted); font-size: 12.5px; }
.link { color: #9db4ff; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(104px, 1fr)); gap: 30px 14px; justify-items: center; }
.app { position: relative; display: flex; flex-direction: column; align-items: center; gap: 9px; width: 100px; }
.app[hidden] { display: none; }
.icon { position: relative; width: 84px; height: 84px; border-radius: 22px; display: grid; place-items: center; background: linear-gradient(160deg, color-mix(in oklab, var(--c), white 22%), var(--c) 55%, color-mix(in oklab, var(--c), black 20%)); box-shadow: inset 0 1px 0 rgba(255,255,255,0.3), 0 12px 26px rgba(0,0,0,0.38); transition: transform 120ms ease; }
.app:hover .icon { transform: translateY(-2px) scale(1.03); }
.badge { font-size: 32px; font-weight: 600; color: rgba(255,255,255,0.96); text-shadow: 0 1px 3px rgba(0,0,0,0.3); }
.label { font-size: 12px; color: rgba(244,246,251,0.92); text-shadow: 0 1px 3px rgba(0,0,0,0.55); max-width: 100px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dot { width: 9px; height: 9px; border-radius: 999px; background: #6f7787; box-shadow: 0 0 0 2px rgba(9,12,20,0.6); }
.icon .dot { position: absolute; right: -2px; bottom: -2px; }
.dot.up { background: #7ad0a3; }
.dot.down { background: #f08a8a; }
.dot.gateway { background: #8b9dff; }
.empty { color: var(--muted); }
@media (max-width: 640px) {
  main { padding: 14px 18px 56px; }
  .find input { width: 150px; }
  .icon { width: 72px; height: 72px; border-radius: 19px; }
  .badge { font-size: 27px; }
  .grid { grid-template-columns: repeat(auto-fill, minmax(84px, 1fr)); gap: 22px 10px; }
  .app, .label { width: 84px; }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn routes(source: &str) -> Vec<Route> {
        let gateway: Gateway = serde_json::from_str(source).unwrap();
        gateway.routes
    }

    #[test]
    fn a_host_prefers_the_myc_name() {
        let route = Route {
            name: "store".into(),
            hosts: vec!["localhost".into(), "store.myc".into()],
            upstream: None,
            service_kind: None,
            spore: None,
        };
        assert_eq!(route.host(), Some("store.myc"));
    }

    #[test]
    fn upstreams_parse_as_one_or_many() {
        let parsed = routes(
            r#"{"routes":[{"name":"a","hosts":["a.myc"],"upstream":"http://127.0.0.1:1"},{"name":"b","hosts":["b.myc"],"upstream":["http://127.0.0.1:2","https://b.example"]}]}"#,
        );
        assert_eq!(
            parsed[0].upstream.as_ref().and_then(Upstream::first),
            Some("http://127.0.0.1:1")
        );
        assert_eq!(
            parsed[1].upstream.as_ref().and_then(Upstream::first),
            Some("http://127.0.0.1:2")
        );
    }

    #[test]
    fn the_launcher_keeps_the_files_order_and_links_every_route() {
        let parsed = routes(
            r#"{"routes":[{"name":"store","hosts":["store.myc"],"upstream":"http://127.0.0.1:1"},{"name":"psst","hosts":["psst.myc"]}]}"#,
        );
        let page = page(&parsed, &StoreState::default(), None);
        let store = page.find("href=\"https://store.myc/\"").unwrap();
        let psst = page.find("href=\"https://psst.myc/\"").unwrap();
        assert!(store < psst, "the routes file decides the order");
    }

    #[test]
    fn a_spore_names_the_mark_and_the_colour() {
        let parsed = routes(r#"{"routes":[{"name":"term","hosts":["term.myc"]}]}"#);
        let store = StoreState {
            apps: vec![App {
                id: "dev.fungos.terminal".into(),
                name: "terminal".into(),
                badge: Some("T".into()),
                color: Some("#8bd5ca".into()),
            }],
            pending: Pending::default(),
        };
        let page = page(&parsed, &store, None);
        assert!(page.contains("<span class=\"badge\">T</span>"));
        assert!(page.contains("--c:#8bd5ca"));
        assert!(page.contains(">terminal</span>"));
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(escape("<b> & \"q\""), "&lt;b&gt; &amp; &quot;q&quot;");
    }
}
