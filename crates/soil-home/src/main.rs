//! `soil-home`: the front door for the names this machine serves.
//!
//! The Mycelium gateway owns DNS, TLS and routing; this only answers `home.myc`
//! with the routes in `~/.mycelium/gateway/routes.json`. It reads that file per
//! request, so a route added to the gateway shows up without restarting
//! anything, and it probes each local upstream so "it is running" is visible
//! rather than implied.
//!
//! The order is the file's order: an operator who wants Store first writes it
//! first. Nothing here curates names or descriptions.
//!
//! The palette matches `soil-store`'s presenter on purpose: these are two pages
//! of one front door, not two products.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const DEFAULT_ADDR: &str = "127.0.0.1:8801";
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

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
    let address = named(&args, "--addr").unwrap_or_else(|| DEFAULT_ADDR.to_owned());
    let listener =
        TcpListener::bind(&address).map_err(|error| format!("listen {address}: {error}"))?;
    match listener.local_addr() {
        Ok(bound) => eprintln!("soil-home: http://{bound}/ reading {}", routes.display()),
        Err(_) => eprintln!("soil-home: http://{address}/ reading {}", routes.display()),
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let routes = routes.clone();
        std::thread::spawn(move || {
            let _ = answer(stream, &routes);
        });
    }
    Ok(())
}

fn default_routes() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".mycelium/gateway/routes.json")
}

fn named(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn usage() -> String {
    "usage: soil-home [--routes PATH] [--addr HOST:PORT]\n       soil-home --version".into()
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

fn answer(mut stream: TcpStream, routes_path: &Path) -> std::io::Result<()> {
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
        ("GET", "/healthz") => ("200 OK", TEXT, "ok\n".to_owned()),
        ("GET", "/api/routes") => match read_routes(routes_path) {
            Ok(routes) => ("200 OK", JSON, routes_json(&routes)),
            Err(error) => ("503 Service Unavailable", TEXT, format!("{error}\n")),
        },
        ("GET", "/") => match read_routes(routes_path) {
            Ok(routes) => ("200 OK", HTML, page(&routes)),
            Err(error) => (
                "503 Service Unavailable",
                HTML,
                document(&format!(
                    "<main><p class=\"error\">{}</p></main>",
                    escape(&error)
                )),
            ),
        },
        _ => ("404 Not Found", TEXT, "not found\n".to_owned()),
    };

    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
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

fn page(routes: &[Route]) -> String {
    let statuses = statuses(routes);
    let mut cards = String::new();
    for (route, status) in routes.iter().zip(statuses) {
        let host = route.host().unwrap_or_default();
        let kind = route
            .service_kind
            .as_deref()
            .map(|kind| format!("<span class=\"kind\">{}</span>", escape(kind)))
            .unwrap_or_default();
        cards.push_str(&format!(
            r#"<a class="card" href="https://{host}/" data-search="{needle}"><div class="row"><span class="name">{name}</span><span class="dot {status}"></span></div><div class="host">{host}{kind}</div></a>"#,
            host = escape(host),
            needle = escape(&format!("{} {}", route.name, host).to_lowercase()),
            name = escape(&route.name),
            status = status,
            kind = kind,
        ));
    }
    if cards.is_empty() {
        cards.push_str("<p class=\"empty\">No routes are configured yet.</p>");
    }
    let up = routes.iter().filter(|route| probe(route) == "up").count();
    let down = routes.iter().filter(|route| probe(route) == "down").count();
    let body = format!(
        r#"<header><div><h1>fungOS</h1><p class="subtle">{count} names this machine answers</p></div><input id="search" type="search" placeholder="search names" autocomplete="off" autofocus></header><main><section class="grid" id="grid">{cards}</section><p class="empty" id="none" hidden>Nothing matches that name.</p></main><footer><span><span class="dot up"></span>{up} up</span> <span><span class="dot down"></span>{down} down</span> · read from <code>~/.mycelium/gateway/routes.json</code> · TLS and DNS are the Mycelium gateway's</footer><script>{script}</script>"#,
        count = routes.len(),
        up = up,
        down = down,
        cards = cards,
        script = SCRIPT,
    );
    document(&body)
}

fn document(body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>fungOS</title><style>{STYLE}</style></head><body>{body}</body></html>"#
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

/// Filtering happens in the page: the directory is small, and a round trip per
/// keystroke would make search feel slower than the eye.
const SCRIPT: &str = r#"
const search = document.getElementById('search');
const cards = [...document.querySelectorAll('#grid .card')];
const none = document.getElementById('none');
search.addEventListener('input', () => {
  const needle = search.value.trim().toLowerCase();
  let shown = 0;
  for (const card of cards) {
    const match = !needle || card.dataset.search.includes(needle);
    card.hidden = !match;
    if (match) shown += 1;
  }
  none.hidden = shown > 0;
});
"#;

const STYLE: &str = r#"
:root { color-scheme: dark; }
* { box-sizing: border-box; }
body { margin: 0; background: #0f1115; color: #e7e9ee; font: 15px/1.5 ui-sans-serif, -apple-system, system-ui, sans-serif; }
header { display: flex; justify-content: space-between; align-items: end; gap: 24px; padding: 28px 32px 20px; border-bottom: 1px solid #232733; }
h1 { margin: 0; font-size: 26px; font-weight: 600; letter-spacing: -0.02em; }
a { color: inherit; text-decoration: none; }
main { padding: 24px 32px 48px; }
.subtle { margin: 4px 0 0; color: #9aa3b2; font-size: 13px; }
input[type=search] { width: 260px; padding: 9px 12px; border: 1px solid #232733; border-radius: 10px; background: #151922; color: inherit; font: inherit; font-size: 14px; outline: none; }
input[type=search]:focus { border-color: #3a4160; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 14px; }
.card { padding: 16px; border: 1px solid #232733; border-radius: 12px; background: #151922; }
.card:hover { border-color: #3a4160; }
.card[hidden] { display: none; }
.row { display: flex; justify-content: space-between; align-items: center; gap: 8px; }
.name { font-weight: 600; }
.host { margin-top: 2px; color: #8b9dff; font-size: 12px; }
.kind { margin-left: 8px; color: #6f7787; }
.dot { display: inline-block; width: 8px; height: 8px; border-radius: 999px; background: #6f7787; }
.dot.up { background: #7ad0a3; }
.dot.down { background: #f08a8a; }
.dot.gateway { background: #8b9dff; }
footer { padding: 16px 32px 28px; color: #6f7787; font-size: 12px; border-top: 1px solid #232733; }
footer .dot { margin-right: 6px; }
.empty, .error { color: #9aa3b2; }
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
    fn the_page_keeps_the_files_order_and_links_every_route() {
        let parsed = routes(
            r#"{"routes":[{"name":"store","hosts":["store.myc"],"upstream":"http://127.0.0.1:1"},{"name":"psst","hosts":["psst.myc"]}]}"#,
        );
        let page = page(&parsed);
        let store = page.find("href=\"https://store.myc/\"").unwrap();
        let psst = page.find("href=\"https://psst.myc/\"").unwrap();
        assert!(store < psst, "the routes file decides the order");
        assert!(page.contains("data-search=\"store store.myc\""));
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(escape("<b> & \"q\""), "&lt;b&gt; &amp; &quot;q&quot;");
    }
}
