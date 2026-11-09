//! `soil-home`: the front door for the names this machine serves.
//!
//! The Mycelium gateway owns DNS, TLS and routing; this only answers `home.myc`
//! with the routes in `~/.mycelium/gateway/routes.json`, drawn as a launcher:
//! wallpaper, a status bar, widgets, a grid of app tiles and a dock. It reads
//! the route file per request, so a new route appears without restarting
//! anything, and it reads the store for each Spore's mark, its summary, the
//! catalog and the pending-update classes.
//!
//! The route file decides the order and the dock. Nothing here curates names or
//! descriptions; an unknown surface gets a hashed colour and a generic glyph.
//!
//! The palette matches `soil-store`'s presenter on purpose: these are two pages
//! of one front door, not two products.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_ADDR: &str = "127.0.0.1:8801";
const STORE_ADDR: &str = "127.0.0.1:8800";
const PROBE_TIMEOUT: Duration = Duration::from_millis(220);
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
    if args.first().map(String::as_str) == Some("snapshot") {
        return snapshot(&args[1..]);
    }

    let routes = named(&args, "--routes").map_or_else(default_routes, PathBuf::from);
    let wallpaper = named(&args, "--wallpaper")
        .map(PathBuf::from)
        .or_else(|| default_wallpaper().filter(|path| path.is_file()));
    let icons = named(&args, "--icons")
        .map(PathBuf::from)
        .or_else(|| default_icons().filter(|path| path.is_dir()));
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
    if let Some(icons) = &icons {
        eprintln!("soil-home: icons {}", icons.display());
    }

    let cache = Arc::new(Mutex::new(Cache::default()));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let routes = routes.clone();
        let cache = Arc::clone(&cache);
        let wallpaper = wallpaper.clone();
        let icons = icons.clone();
        std::thread::spawn(move || {
            let _ = answer(
                stream,
                &routes,
                &cache,
                wallpaper.as_deref(),
                icons.as_deref(),
            );
        });
    }
    Ok(())
}

fn default_routes() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".mycelium/gateway/routes.json")
}

fn default_wallpaper() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let chosen = PathBuf::from(&home).join(".local/share/fungos/home/wallpaper");
    if chosen.is_file() {
        return Some(chosen);
    }
    // Canvas' bundled default, "Undergrowth": the same art the desktop shows
    // until somebody picks something else.
    let canvas =
        PathBuf::from(home).join("src/fungOS/canvas-native/config/appearance/undergrowth.png");
    canvas.is_file().then_some(canvas)
}

/// A theme's icons, kept on this machine. Nothing from a theme is vendored:
/// a person points this at marks they have the right to use.
fn default_icons() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".local/share/fungos/home/icons"))
}

fn named(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn usage() -> String {
    "usage: soil-home [--routes PATH] [--wallpaper PATH] [--icons DIR] [--addr HOST:PORT]\n       soil-home snapshot [--routes PATH] [-o FILE]\n       soil-home --version".into()
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
    /// Whether the surface belongs in the dock rather than the grid.
    #[serde(default)]
    dock: bool,
    /// `service` for a backend with no UI of its own: it belongs in the status
    /// rail, not the grid. Anything else is an app to open.
    #[serde(default)]
    role: Option<String>,
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

    fn all(&self) -> Vec<&str> {
        match self {
            Self::One(url) => vec![url.as_str()],
            Self::Many(urls) => urls.iter().map(String::as_str).collect(),
        }
    }
}

impl Route {
    fn is_service(&self) -> bool {
        self.role.as_deref() == Some("service")
    }

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
    color: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct Update {
    #[serde(default)]
    id: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct Plan {
    #[serde(default)]
    updates: Vec<Update>,
    #[serde(default)]
    current: Vec<String>,
    #[serde(default)]
    yanked: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct Source {
    #[serde(default)]
    name: String,
    #[serde(default)]
    verified: bool,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct StoreState {
    #[serde(default)]
    apps: Vec<App>,
    #[serde(default = "default_source")]
    source: Source,
    #[serde(default)]
    updates: Plan,
}

fn default_source() -> Source {
    Source::default()
}

/// The machine's own numbers. Absent fields are simply not shown: a desktop
/// has no battery, and a sandbox may have no `sysctl`.
#[derive(Debug, Clone, Default)]
struct System {
    host: Option<String>,
    load: Option<String>,
    memory: Option<String>,
    disk: Option<String>,
    battery: Option<String>,
}

#[derive(Default)]
struct Cache {
    at: Option<Instant>,
    state: StoreState,
    system: System,
}

impl Cache {
    fn take(&mut self) -> (StoreState, System) {
        if let Some(at) = self.at
            && at.elapsed() < STORE_TTL
        {
            return (self.state.clone(), self.system.clone());
        }
        self.state = fetch_store();
        self.system = read_system();
        self.at = Some(Instant::now());
        (self.state.clone(), self.system.clone())
    }
}

fn read_system() -> System {
    System {
        host: command("hostname", &[])
            .map(|host| host.split('.').next().unwrap_or(&host).to_owned()),
        load: command("sysctl", &["-n", "vm.loadavg"]).and_then(|load| parse_load(&load)),
        memory: read_memory(),
        disk: command("df", &["-k", "/"]).and_then(|df| parse_disk(&df)),
        battery: command("pmset", &["-g", "batt"]).and_then(|battery| parse_battery(&battery)),
    }
}

fn command(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn parse_load(source: &str) -> Option<String> {
    let values: Vec<String> = source
        .trim_matches(|c: char| !c.is_ascii_digit() && c != '.')
        .split_whitespace()
        .take(3)
        .filter_map(|value| value.parse::<f64>().ok())
        .map(|value| format!("{value:.0}"))
        .collect();
    (values.len() == 3).then(|| values.join(" · "))
}

fn read_memory() -> Option<String> {
    let total = command("sysctl", &["-n", "hw.memsize"])?
        .parse::<u64>()
        .ok()?;
    let stats = command("vm_stat", &[])?;
    let page = stats
        .lines()
        .next()
        .and_then(|line| line.split("page size of ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|size| size.parse::<u64>().ok())
        .unwrap_or(16_384);
    // Free pages alone undersell a Mac: inactive and speculative pages are
    // reclaimable, and reporting them as used looks like a memory leak.
    let mut available = 0u64;
    for label in ["Pages free", "Pages inactive", "Pages speculative"] {
        if let Some(value) = stats
            .lines()
            .find(|line| line.starts_with(label))
            .and_then(|line| line.split(':').nth(1))
            .and_then(|value| value.trim().trim_end_matches('.').parse::<u64>().ok())
        {
            available += value;
        }
    }
    Some(format!(
        "{} / {} GB",
        available.saturating_mul(page) / 1_073_741_824,
        total / 1_073_741_824
    ))
}

fn parse_disk(source: &str) -> Option<String> {
    let line = source.lines().nth(1)?;
    let columns: Vec<&str> = line.split_whitespace().collect();
    let free = columns.get(3)?.parse::<u64>().ok()?;
    Some(format!("{} GB", free / 1_048_576))
}

fn parse_battery(source: &str) -> Option<String> {
    if !source.contains("InternalBattery") {
        return None;
    }
    let percent = source.split_whitespace().find(|word| word.ends_with('%'))?;
    let state = if source.contains("charging") {
        "charging"
    } else if source.contains("discharging") {
        "on battery"
    } else {
        "plugged in"
    };
    Some(format!("{percent} {state}"))
}

fn fetch_store() -> StoreState {
    let mut state = match get(STORE_ADDR, "/api/apps") {
        Ok(body) => serde_json::from_str::<StoreState>(&body).unwrap_or_default(),
        Err(_) => return StoreState::default(),
    };
    if let Ok(body) = get(STORE_ADDR, "/api/plan")
        && let Ok(plan) = serde_json::from_str::<Plan>(&body)
    {
        state.updates = plan;
    }
    state
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

// ---- snapshot -------------------------------------------------------------

/// The Lua table the home Spore draws.
///
/// This is the same data the HTML presenter renders: routes become app and
/// service entries, the store contributes marks and the catalog, and the
/// machine contributes its metrics. The Spore never fetches; it receives this
/// file from the host.
fn snapshot(args: &[String]) -> Result<(), String> {
    let routes = read_routes(&named(args, "--routes").map_or_else(default_routes, PathBuf::from))?;
    let store = fetch_store();
    let system = read_system();
    let probes = probes(&routes);

    let mut lua = String::from("return {\n");
    lua.push_str(&format!(
        "  host = {},\n  wallpaper = \"undergrowth\",\n",
        lua_string(system.host.as_deref().unwrap_or("fungOS"))
    ));

    let mut apps = String::new();
    let mut served: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut dock = Vec::new();
    for route in &routes {
        if route.name == "home" || route.is_service() {
            continue;
        }
        let app = mark(route, &store.apps);
        let id = app
            .map(|app| app.id.clone())
            .unwrap_or_else(|| route.name.clone());
        served.insert(app.map(|app| app.id.clone()).unwrap_or_default());
        let name = app
            .map(|app| app.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| route.name.clone());
        let category = app
            .and_then(|app| app.category.clone())
            .unwrap_or_else(|| "app".into());
        let color = app
            .and_then(|app| app.color.clone())
            .unwrap_or_else(|| hashed_color(&route.name));
        apps.push_str(&format!(
            "    {{ id = {}, name = {}, glyph = {}, host = {}, category = {}, color = {}, state = \"installed\" }},\n",
            lua_string(&id),
            lua_string(&name),
            lua_string(&route.name),
            lua_string(route.host().unwrap_or("")),
            lua_string(&category),
            lua_string(&color),
        ));
        if route.dock {
            dock.push(id);
        }
    }
    for app in &store.apps {
        if served.contains(&app.id) {
            continue;
        }
        let name = if app.name.is_empty() {
            app.id.rsplit('.').next().unwrap_or(&app.id).to_owned()
        } else {
            app.name.clone()
        };
        let short = app.id.rsplit('.').next().unwrap_or(&app.id);
        apps.push_str(&format!(
            "    {{ id = {}, name = {}, glyph = {}, host = \"store.myc\", category = {}, color = {}, state = \"installed\" }},\n",
            lua_string(&app.id),
            lua_string(&name),
            lua_string(short),
            lua_string(app.category.as_deref().unwrap_or("app")),
            lua_string(&app.color.clone().unwrap_or_else(|| hashed_color(&name))),
        ));
    }
    lua.push_str(&format!("  apps = {{\n{apps}  }},\n"));

    let mut services = String::new();
    for (route, probe) in routes.iter().zip(&probes) {
        if !route.is_service() {
            continue;
        }
        services.push_str(&format!(
            "    {{ name = {}, host = {}, status = {}, ms = {} }},\n",
            lua_string(&route.name),
            lua_string(route.host().unwrap_or("")),
            lua_string(probe.status),
            probe
                .ms
                .map(|ms| ms.to_string())
                .unwrap_or_else(|| "nil".into()),
        ));
    }
    lua.push_str(&format!("  services = {{\n{services}  }},\n"));

    let up = probes.iter().filter(|probe| probe.status == "up").count();
    let down: Vec<&str> = routes
        .iter()
        .zip(&probes)
        .filter(|(_, probe)| probe.status == "down")
        .map(|(route, _)| route.name.as_str())
        .collect();
    let mut latencies: Vec<u128> = probes.iter().filter_map(|probe| probe.ms).collect();
    latencies.sort_unstable();
    let median = latencies
        .get(latencies.len() / 2)
        .map(|ms| format!("{ms} ms median"))
        .unwrap_or_else(|| "no latencies".to_owned());
    let down_text = if down.is_empty() {
        "everything answering".to_owned()
    } else {
        format!("down: {}", down.join(", "))
    };
    let time = command("date", &["+%H:%M"]).unwrap_or_default();
    let date = command("date", &["+%A, %B %-d"]).unwrap_or_default();
    let day = command("date", &["+%j"]).unwrap_or_default();
    let year = command("date", &["+%Y"]).unwrap_or_default();

    let mut classes = String::new();
    for class in ["live", "restart", "consent", "system", "hold"] {
        let count = store
            .updates
            .updates
            .iter()
            .filter(|update| update.class == class)
            .count();
        if count > 0 {
            let ink = match class {
                "live" => "success",
                "restart" => "info",
                "consent" => "warning",
                "system" => "danger",
                _ => "muted",
            };
            classes.push_str(&format!(
                "        {{ name = {}, count = {count}, ink = {} }},\n",
                lua_string(class),
                lua_string(ink)
            ));
        }
    }

    lua.push_str(&format!(
        "  widgets = {{\n    today = {{ time = {}, date = {}, day = {} }},\n    machine = {{ up = {up}, total = {}, median = {}, down = {} }},\n    store = {{ spores = {}, source = {}, current = {}, yanked = {}, classes = {{\n{classes}    }} }},\n    system = {{ load = {}, memory = {}, disk = {}, battery = {} }},\n  }},\n",
        lua_string(&time),
        lua_string(&date),
        lua_string(&format!("day {day} of {year}")),
        routes.len(),
        lua_string(&median),
        lua_string(&down_text),
        store.apps.len(),
        lua_string(&format!(
            "{} · {}",
            store.source.name,
            if store.source.verified {
                "signed index"
            } else {
                "no signed index"
            }
        )),
        store.updates.current.len(),
        store.updates.yanked.len(),
        lua_string(system.load.as_deref().unwrap_or("")),
        lua_string(system.memory.as_deref().unwrap_or("")),
        lua_string(system.disk.as_deref().unwrap_or("")),
        lua_string(system.battery.as_deref().unwrap_or("")),
    ));

    let docked: Vec<String> = dock.iter().map(|id| lua_string(id)).collect();
    lua.push_str(&format!("  dock = {{ {} }},\n}}\n", docked.join(", ")));

    match named(args, "-o") {
        Some(path) => std::fs::write(&path, lua).map_err(|error| format!("{path}: {error}"))?,
        None => print!("{lua}"),
    }
    Ok(())
}

fn lua_string(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---- probes ---------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Probe {
    status: &'static str,
    ms: Option<u128>,
}

/// Whether any of the route's upstreams accepts a connection right now, and how
/// long the fastest one took. A route with a dead local upstream but a live
/// fallback is up, which is what a person sees when they open the name.
fn probe(route: &Route) -> Probe {
    let Some(upstream) = &route.upstream else {
        return Probe {
            status: if route.service_kind.is_some() {
                "gateway"
            } else {
                "unknown"
            },
            ms: None,
        };
    };
    let mut best: Option<u128> = None;
    for url in upstream.all() {
        let (rest, port) = if let Some(rest) = url.strip_prefix("http://") {
            (rest, 80)
        } else if let Some(rest) = url.strip_prefix("https://") {
            (rest, 443)
        } else {
            continue;
        };
        let host = rest.split('/').next().unwrap_or(rest);
        let address = match host.parse::<SocketAddr>() {
            Ok(address) => address,
            Err(_) => match (host, port)
                .to_socket_addrs()
                .ok()
                .and_then(|mut it| it.next())
            {
                Some(address) => address,
                None => continue,
            },
        };
        let started = Instant::now();
        if TcpStream::connect_timeout(&address, PROBE_TIMEOUT).is_ok() {
            let elapsed = started.elapsed().as_millis();
            best = Some(best.map_or(elapsed, |best: u128| best.min(elapsed)));
        }
    }
    Probe {
        status: if best.is_some() { "up" } else { "down" },
        ms: best,
    }
}

/// One probe per route, in parallel, so a stopped upstream costs one timeout and
/// not one per tile.
fn probes(routes: &[Route]) -> Vec<Probe> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = routes
            .iter()
            .map(|route| scope.spawn(move || probe(route)))
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or(Probe {
                    status: "unknown",
                    ms: None,
                })
            })
            .collect()
    })
}

// ---- pages ----------------------------------------------------------------

fn answer(
    mut stream: TcpStream,
    routes_path: &Path,
    cache: &Mutex<Cache>,
    wallpaper: Option<&Path>,
    icons: Option<&Path>,
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
                let (store, system) = cache
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .take();
                (
                    "200 OK",
                    HTML,
                    page(&routes, &store, &system, wallpaper, icons).into_bytes(),
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
        ("GET", other) if other.starts_with("/icon/") => {
            let name = &other[6..];
            let safe = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            match (safe, icons) {
                (true, Some(dir)) => match icon_for(dir, name) {
                    Some(path) => match std::fs::read(&path) {
                        Ok(bytes) => ("200 OK", icon_type(&path), bytes),
                        Err(_) => ("404 Not Found", TEXT, b"no icon\n".to_vec()),
                    },
                    None => ("404 Not Found", TEXT, b"no icon\n".to_vec()),
                },
                _ => ("404 Not Found", TEXT, b"no icon\n".to_vec()),
            }
        }
        ("GET", "/wallpaper") => match wallpaper {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => ("200 OK", icon_type(path), bytes),
                Err(_) => ("404 Not Found", TEXT, b"no wallpaper\n".to_vec()),
            },
            None => ("404 Not Found", TEXT, b"no wallpaper\n".to_vec()),
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

/// The file a theme provides for one surface, if it does.
fn icon_for(dir: &Path, name: &str) -> Option<PathBuf> {
    for extension in ["png", "svg", "webp", "jpg", "jpeg"] {
        let path = dir.join(format!("{name}.{extension}"));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn icon_type(path: &Path) -> &'static str {
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
                "status": probe(route).status,
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

/// A descriptive glyph per surface. These are stroke paths on a 24-unit grid,
/// so a tile reads as "terminal" before anyone reads its label.
fn glyph(name: &str) -> &'static str {
    match name {
        "store" => {
            r#"<path d="M6 2 3 6v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V6l-3-4z"/><path d="M3 6h18"/><path d="M16 10a4 4 0 0 1-8 0"/>"#
        }
        "term" | "terminal" => r#"<path d="m4 17 6-6-6-6"/><path d="M12 19h8"/>"#,
        "wiki" => {
            r#"<path d="M12 7v14"/><path d="M3 18a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h5a4 4 0 0 1 4 4 4 4 0 0 1 4-4h5a1 1 0 0 1 1 1v13a1 1 0 0 1-1 1h-6a3 3 0 0 0-3 3 3 3 0 0 0-3-3z"/>"#
        }
        "worklog" => r#"<path d="M4 4h16v16H4z"/><path d="M8 9h8"/><path d="M8 13h5"/>"#,
        "ai" => {
            r#"<path d="m12 3 1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z"/><path d="M19 3v4"/><path d="M21 5h-4"/>"#
        }
        "mcp" => {
            r#"<path d="M12 22v-5"/><path d="M9 8V2"/><path d="M15 8V2"/><path d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8z"/>"#
        }
        "sfx" => {
            r#"<path d="M11 5 6 9H2v6h4l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M18.5 5.5a9 9 0 0 1 0 13"/>"#
        }
        "webhook" | "confluence" => {
            r#"<path d="M17.5 19a4.5 4.5 0 0 0 0-9 6 6 0 0 0-11.6-1.7A4 4 0 0 0 6 19z"/>"#
        }
        "manavault" => {
            r#"<rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V8a5 5 0 0 1 10 0v3"/>"#
        }
        "home" => r#"<path d="M3 11 12 3l9 8"/><path d="M5 10v10h14V10"/>"#,
        "mush" => r#"<path d="M4 14a8 8 0 0 1 16 0z"/><path d="M12 14v6"/>"#,
        "chrome" | "spore-browser" => {
            r#"<circle cx="12" cy="12" r="9"/><path d="M3 9h18"/><path d="M3 15h18"/><path d="M12 3a15 15 0 0 1 0 18"/><path d="M12 3a15 15 0 0 0 0 18"/>"#
        }
        "contacts" => {
            r#"<circle cx="9" cy="8" r="3.4"/><path d="M2.5 20a6.5 6.5 0 0 1 13 0"/><path d="M16 8a3 3 0 1 1 0 6"/><path d="M17.5 20a6 6 0 0 0-2-4.5"/>"#
        }
        "crm" => {
            r#"<rect x="3" y="4" width="5" height="16" rx="1.2"/><rect x="10" y="4" width="5" height="10" rx="1.2"/><rect x="17" y="4" width="4" height="13" rx="1.2"/>"#
        }
        "dfm" | "dfm-report" => {
            r#"<path d="M4 20V4"/><path d="M4 20h16"/><path d="M8 16v-4"/><path d="M13 16V8"/><path d="M18 16v-6"/>"#
        }
        "gallery" => {
            r#"<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="8.5" cy="9.5" r="1.6"/><path d="m3 17 5-4 4 3 3-2 6 5"/>"#
        }
        "git" => {
            r#"<circle cx="6" cy="6" r="2.4"/><circle cx="6" cy="18" r="2.4"/><circle cx="18" cy="9" r="2.4"/><path d="M6 8.4v7.2"/><path d="M18 11.4c0 4-4 3.1-6 4.6"/>"#
        }
        "inbox" => r#"<path d="M3 12h5l2 3h4l2-3h5"/><path d="M5 4h14l2 8v8H3v-8z"/>"#,
        "media-probe" => r#"<path d="M3 12h3l2-5 3 10 3-14 3 16 2-7h2"/>"#,
        "notify" => {
            r#"<path d="M6 16V9a6 6 0 0 1 12 0v7l2 3H4z"/><path d="M10 21a2 2 0 0 0 4 0"/>"#
        }
        "search" => r#"<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>"#,
        "consent" => {
            r#"<path d="M12 3 4 6v6c0 4.5 3.4 8 8 9 4.6-1 8-4.5 8-9V6z"/><path d="m8.5 12 2.5 2.5 4.5-5"/>"#
        }
        "surface-probe" => {
            r#"<path d="M4 8V4h4"/><path d="M16 4h4v4"/><path d="M20 16v4h-4"/><path d="M8 20H4v-4"/><circle cx="12" cy="12" r="2.6"/>"#
        }
        "rooms" => {
            r#"<rect x="3" y="4" width="8" height="7" rx="1.4"/><rect x="13" y="4" width="8" height="7" rx="1.4"/><rect x="3" y="13" width="8" height="7" rx="1.4"/><rect x="13" y="13" width="8" height="7" rx="1.4"/>"#
        }
        _ => {
            r#"<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>"#
        }
    }
}

fn page(
    routes: &[Route],
    store: &StoreState,
    system: &System,
    wallpaper: Option<&Path>,
    icons: Option<&Path>,
) -> String {
    let probes = probes(routes);
    let mut tiles = String::new();
    let mut docked = String::new();
    let mut services = String::new();

    for (route, probe) in routes.iter().zip(&probes) {
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
        let latency = probe
            .ms
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "no answer".to_owned());

        if route.is_service() {
            services.push_str(&format!(
                r#"<li title="{label} — {host}"><span class="dot {status}"></span><span class="svc-name">{label}</span><span class="svc-meta">{latency}</span></li>"#,
                status = probe.status,
                label = escape(&label),
                host = escape(host),
                latency = escape(&latency),
            ));
            continue;
        }

        let color = app
            .and_then(|app| app.color.clone())
            .unwrap_or_else(|| hashed_color(&route.name));
        let art = icons
            .and_then(|dir| icon_for(dir, &route.name))
            .map(|_| {
                format!(
                    "<img class=\"art\" src=\"/icon/{}\" alt=\"\">",
                    escape(&route.name)
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "<svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.6\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{}</svg>",
                    glyph(&route.name)
                )
            });
        let kind = route
            .service_kind
            .as_deref()
            .map(|kind| format!(" · {kind}"))
            .unwrap_or_default();
        let description = app
            .and_then(|app| app.summary.clone())
            .filter(|summary| !summary.is_empty())
            .unwrap_or_else(|| format!("{} on this machine", host));
        let version = app
            .and_then(|app| app.version.clone())
            .filter(|version| !version.is_empty())
            .map(|version| format!(" · v{version}"))
            .unwrap_or_default();
        let sub = app
            .and_then(|app| app.category.clone())
            .or_else(|| route.service_kind.clone())
            .unwrap_or_else(|| "app".to_owned());
        let needle = format!("{} {} {}", route.name, label, host).to_lowercase();
        let tile = format!(
            r#"<a class="app" href="https://{host}/" data-search="{needle}" title="{label} · {host}"><span class="peek"><span class="peek-name">{label}</span><span class="peek-host">{host}{kind}{version}</span><span class="peek-meta"><span class="dot {status}"></span>{status} · {latency}</span><span class="peek-desc">{description}</span></span><span class="icon" style="--c:{color}">{art}<span class="dot {status}"></span></span><span class="label">{label}</span><span class="sub">{sub}</span></a>"#,
            host = escape(host),
            needle = escape(&needle),
            color = escape(&color),
            art = art,
            status = probe.status,
            latency = escape(&latency),
            kind = escape(&kind),
            version = escape(&version),
            label = escape(&label),
            sub = escape(&sub),
            description = escape(&description),
        );
        tiles.push_str(&tile);
        if route.dock {
            docked.push_str(&tile);
        }
    }
    let mut served: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for route in routes {
        if let Some(app) = mark(route, &store.apps) {
            served.insert(app.id.as_str());
        }
    }
    for app in &store.apps {
        if served.contains(app.id.as_str()) {
            continue;
        }
        let name = if app.name.is_empty() {
            app.id.rsplit('.').next().unwrap_or(&app.id).to_owned()
        } else {
            app.name.clone()
        };
        let color = app.color.clone().unwrap_or_else(|| hashed_color(&name));
        let sub = app.category.clone().unwrap_or_else(|| "Spore".to_owned());
        let description = app
            .summary
            .clone()
            .filter(|summary| !summary.is_empty())
            .unwrap_or_else(|| "Listed in the store".to_owned());
        let version = app
            .version
            .clone()
            .filter(|version| !version.is_empty())
            .map(|version| format!(" · v{version}"))
            .unwrap_or_default();
        let short = app.id.rsplit('.').next().unwrap_or(&app.id);
        let needle = format!("{} {} {}", name, short, sub).to_lowercase();
        tiles.push_str(&format!(
            r#"<a class="app" href="https://store.myc/spore/{id}" data-search="{needle}" title="{name} · in the store"><span class="peek"><span class="peek-name">{name}</span><span class="peek-host">store.myc{version}</span><span class="peek-meta"><span class="dot gateway"></span>in the store</span><span class="peek-desc">{description}</span></span><span class="icon" style="--c:{color}"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">{glyph}</svg><span class="dot gateway"></span></span><span class="label">{name}</span><span class="sub">{sub}</span></a>"#,
            id = escape(&app.id),
            needle = escape(&needle),
            name = escape(&name),
            color = escape(&color),
            sub = escape(&sub),
            version = escape(&version),
            description = escape(&description),
            glyph = glyph(short),
        ));
    }
    if tiles.is_empty() {
        tiles.push_str("<p class=\"empty\">No apps are configured yet.</p>");
    }
    if services.is_empty() {
        services.push_str("<li class=\"muted\">No services configured.</li>");
    }

    let up = probes.iter().filter(|probe| probe.status == "up").count();
    let down: Vec<&str> = routes
        .iter()
        .zip(&probes)
        .filter(|(_, probe)| probe.status == "down")
        .map(|(route, _)| route.name.as_str())
        .collect();
    let mut latencies: Vec<u128> = probes.iter().filter_map(|probe| probe.ms).collect();
    latencies.sort_unstable();
    let median = latencies
        .get(latencies.len() / 2)
        .map(|ms| format!("{ms} ms median"))
        .unwrap_or_else(|| "no latencies".to_owned());
    let down_list = if down.is_empty() {
        "everything answering".to_owned()
    } else {
        format!("down: {}", down.join(", "))
    };

    let updates = store.updates.updates.len();
    let mut classes = String::new();
    for class in ["live", "restart", "consent", "system", "hold"] {
        let count = store
            .updates
            .updates
            .iter()
            .filter(|update| update.class == class)
            .count();
        if count > 0 {
            classes.push_str(&format!(
                "<span class=\"chip {class}\">{count} {class}</span>",
                class = class
            ));
        }
    }
    if classes.is_empty() {
        classes.push_str("<span class=\"muted\">every Spore is current</span>");
    }
    let mut pending = String::new();
    for update in store.updates.updates.iter().take(3) {
        pending.push_str(&format!(
            "<li><span>{}</span><span>{} → {}</span></li>",
            escape(&update.id),
            escape(&update.from),
            escape(&update.to)
        ));
    }
    if pending.is_empty() {
        pending.push_str("<li class=\"muted\"><span>nothing waiting</span></li>");
    }
    let pending_block = if updates == 0 {
        String::new()
    } else {
        format!("<ul class=\"rows\">{pending}</ul>")
    };
    let store_link = if updates == 0 {
        "open the store".to_owned()
    } else {
        format!("{updates} updates · see all")
    };
    let source = if store.apps.is_empty() {
        "the store is not answering".to_owned()
    } else {
        format!(
            "{} · {}",
            escape(&store.source.name),
            if store.source.verified {
                "signed index"
            } else {
                "no signed index"
            }
        )
    };

    let mut categories: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for app in &store.apps {
        if let Some(category) = app.category.as_deref() {
            *categories.entry(category).or_default() += 1;
        }
    }
    let mut categories: Vec<(&str, usize)> = categories.into_iter().collect();
    categories.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mut catalog = String::new();
    for (category, count) in categories.iter().take(4) {
        catalog.push_str(&format!(
            "<li><span>{}</span><span>{}</span></li>",
            escape(category),
            count
        ));
    }
    if catalog.is_empty() {
        catalog.push_str("<li class=\"muted\">no categories</li>");
    }

    let mut metrics = String::new();
    for (label, value) in [
        ("load", system.load.as_deref()),
        ("memory", system.memory.as_deref()),
        ("disk", system.disk.as_deref()),
        ("battery", system.battery.as_deref()),
    ] {
        if let Some(value) = value {
            metrics.push_str(&format!(
                "<li><span>{label}</span><span>{}</span></li>",
                escape(value)
            ));
        }
    }
    if metrics.is_empty() {
        metrics.push_str("<li class=\"muted\"><span>no metrics</span></li>");
    }
    let who = system
        .host
        .as_deref()
        .map(escape)
        .unwrap_or_else(|| "fungOS".to_owned());

    let body = format!(
        r#"<header class="bar"><div class="when"><span id="clock">--:--</span><span id="day">—</span></div><div class="who">{who}</div><div class="find"><input id="search" type="search" placeholder="Search apps" autocomplete="off"></div></header><main><section class="widgets"><article class="widget"><span class="key">Today</span><p class="big" id="today">—</p><p class="muted" id="today-date"></p><p class="muted" id="today-year"></p></article><article class="widget"><span class="key">This machine</span><p class="big">{up}<span class="muted"> / {count} up</span></p><p class="muted">{median}</p><p class="muted">{down_list}</p></article><article class="widget"><span class="key">Store</span><p class="big">{spores}<span class="muted"> Spores</span></p><p class="muted">{source} · {current} current · {yanked} yanked</p><p class="classes">{classes}</p>{pending_block}<p class="key inline-key">Catalog</p><ul class="rows">{catalog}</ul><p class="muted"><a class="link" href="https://store.myc/updates">{store_link}</a></p></article><article class="widget"><span class="key">System</span><ul class="rows">{metrics}</ul></article></section><section class="content"><section class="grid" id="grid">{tiles}</section><aside class="rail"><h2>Services</h2><ul class="services">{services}</ul></aside></section><p class="empty" id="none" hidden>Nothing matches that name.</p></main><footer class="dock"><div class="dock-inner">{docked}</div></footer><script>{script}</script>"#,
        up = up,
        count = routes.len(),
        median = median,
        down_list = escape(&down_list),
        spores = store.apps.len(),
        source = source,
        current = store.updates.current.len(),
        yanked = store.updates.yanked.len(),
        classes = classes,
        pending_block = pending_block,
        store_link = store_link,
        catalog = catalog,
        metrics = metrics,
        who = who,
        tiles = tiles,
        services = services,
        docked = docked,
        script = SCRIPT,
    );
    document(&body, wallpaper.is_some())
}

fn document(body: &str, wall: bool) -> String {
    let class = if wall { " class=\"wall\"" } else { "" };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><meta name="theme-color" content='#0b1020'><title>fungOS</title><link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24'><text y='19' font-size='19'>◧</text></svg>"><style>{STYLE}</style></head><body{class}>{body}</body></html>"#
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
  const time = `${pad(now.getHours())}:${pad(now.getMinutes())}`;
  document.getElementById('clock').textContent = time;
  document.getElementById('day').textContent = now.toLocaleDateString(undefined, { weekday: 'long', month: 'short', day: 'numeric' });
  document.getElementById('today').textContent = time;
  document.getElementById('today-date').textContent = now.toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric' });
  const start = new Date(now.getFullYear(), 0, 0);
  const day = Math.floor((now - start) / 86400000);
  document.getElementById('today-year').textContent = `day ${day} of ${now.getFullYear()}`;
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
body.wall::after { content: ""; position: fixed; inset: 0; z-index: -1; background: linear-gradient(rgba(5,7,13,0.3), rgba(5,7,13,0.62)); }
a { color: inherit; text-decoration: none; }
.bar { display: flex; align-items: center; justify-content: space-between; gap: 18px; padding: calc(14px + env(safe-area-inset-top)) 26px 10px; font-size: 13px; color: var(--muted); }
.when { display: flex; gap: 10px; align-items: baseline; }
#clock { font-weight: 600; color: var(--ink); }
.who { font-weight: 600; letter-spacing: 0.14em; text-transform: uppercase; font-size: 11px; }
.find input { width: 240px; padding: 8px 14px; border-radius: 999px; border: 1px solid rgba(255,255,255,0.16); background: rgba(18,22,34,0.55); color: var(--ink); font: inherit; font-size: 13px; outline: none; backdrop-filter: blur(14px); }
.find input:focus { border-color: rgba(255,255,255,0.38); background: rgba(24,30,46,0.72); }
main { flex: 1; padding: 18px 32px 120px; max-width: 1120px; width: 100%; margin: 0 auto; }
.widgets { display: grid; grid-template-columns: repeat(auto-fit, minmax(196px, 1fr)); gap: 12px; margin-bottom: 26px; }
.widget { padding: 14px 16px; border-radius: 20px; border: 1px solid rgba(255,255,255,0.1); background: rgba(18,22,34,0.5); backdrop-filter: blur(18px); box-shadow: 0 18px 40px rgba(0,0,0,0.28); }
.key.inline-key { margin-top: 10px; }
.key { display: block; font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--muted); }
.big { margin: 5px 0 2px; font-size: 27px; font-weight: 600; letter-spacing: -0.02em; }
.muted { margin: 3px 0 0; color: var(--muted); font-size: 12.5px; }
.link { color: #9db4ff; }
.classes { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0 2px; }
.chip { padding: 2px 8px; border-radius: 999px; font-size: 10.5px; text-transform: uppercase; letter-spacing: 0.06em; background: rgba(255,255,255,0.08); color: var(--muted); }
.chip.live { background: #1d2a22; color: #7ad0a3; }
.chip.restart { background: #1a2130; color: #8b9dff; }
.chip.consent { background: #2a2618; color: #f2c14e; }
.chip.system { background: #2d1a1c; color: #f08a8a; }
.chip.hold { background: #232733; color: #9aa3b2; }
.rows { list-style: none; margin: 6px 0 0; padding: 0; display: grid; gap: 5px; font-size: 12.5px; color: var(--muted); }
.rows li { display: flex; justify-content: space-between; gap: 10px; }
.rows li span:last-child { color: var(--ink); font-variant-numeric: tabular-nums; }
.content { display: grid; grid-template-columns: minmax(0, 1fr) 264px; gap: 22px; align-items: start; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, 108px); justify-content: center; gap: 22px 6px; justify-items: center; }
.rail { padding: 16px 18px; border-radius: 22px; border: 1px solid rgba(255,255,255,0.1); background: rgba(18,22,34,0.5); backdrop-filter: blur(18px); box-shadow: 0 18px 40px rgba(0,0,0,0.28); position: sticky; top: 16px; }
.rail h2 { margin: 0 0 12px; font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--muted); font-weight: 600; }
.services { list-style: none; margin: 0; padding: 0; display: grid; gap: 11px; }
.services li { display: grid; grid-template-columns: 9px 1fr auto; align-items: center; gap: 10px; font-size: 13px; }
.svc-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.svc-meta { color: var(--muted); font-size: 11.5px; font-variant-numeric: tabular-nums; }
.app { position: relative; display: flex; flex-direction: column; align-items: center; gap: 8px; width: 104px; outline: none; }
.app[hidden] { display: none; }
.app:focus-visible .icon { box-shadow: 0 0 0 3px rgba(139,157,255,0.75), 0 14px 30px rgba(0,0,0,0.4); }
.icon { position: relative; width: 78px; height: 78px; border-radius: 20px; display: grid; place-items: center; filter: saturate(1.06); background: linear-gradient(165deg, color-mix(in oklab, var(--c), white 13%), var(--c) 52%, color-mix(in oklab, var(--c), black 27%)); box-shadow: inset 0 1px 0 rgba(255,255,255,0.34), inset 0 -1px 0 rgba(0,0,0,0.18), 0 14px 30px rgba(0,0,0,0.4); transition: transform 120ms ease; }
.app:hover .icon, .app:focus-visible .icon { transform: translateY(-2px) scale(1.03); }
.icon svg { width: 56%; height: 56%; color: rgba(255,255,255,0.96); }
.icon img.art { width: 100%; height: 100%; border-radius: inherit; object-fit: cover; display: block; }
.sub { font-size: 10.5px; color: rgba(244,246,251,0.6); text-shadow: 0 1px 3px rgba(0,0,0,0.6); }
.label { font-size: 12.5px; color: rgba(244,246,251,0.95); text-shadow: 0 1px 4px rgba(0,0,0,0.6); max-width: 104px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.peek { position: absolute; left: 50%; bottom: calc(100% + 10px); width: 230px; transform: translate(-50%, 6px); padding: 12px 14px; border-radius: 16px; border: 1px solid rgba(255,255,255,0.14); background: rgba(16,20,31,0.92); backdrop-filter: blur(18px); box-shadow: 0 20px 44px rgba(0,0,0,0.45); text-align: left; opacity: 0; pointer-events: none; transition: opacity 120ms ease, transform 120ms ease; z-index: 6; }
.app:hover .peek, .app:focus-visible .peek { opacity: 1; transform: translate(-50%, 0); }
.peek-name { display: block; font-size: 13px; font-weight: 600; }
.peek-host { display: block; margin-top: 1px; font-size: 11.5px; color: #8b9dff; }
.peek-meta { display: flex; align-items: center; gap: 6px; margin-top: 6px; font-size: 11.5px; color: var(--muted); }
.peek-desc { display: block; margin-top: 6px; font-size: 12px; color: #c3c9d4; line-height: 1.35; }
.dot { width: 9px; height: 9px; border-radius: 999px; background: #6f7787; box-shadow: 0 0 0 2px rgba(9,12,20,0.6); }
.icon .dot { position: absolute; right: -2px; bottom: -2px; }
.dot.up { background: #7ad0a3; }
.dot.down { background: #f08a8a; }
.dot.gateway { background: #8b9dff; }
.empty { color: var(--muted); }
.dock { position: fixed; left: 0; right: 0; bottom: 0; display: flex; justify-content: center; padding: 10px 22px calc(14px + env(safe-area-inset-bottom)); pointer-events: none; z-index: 7; }
.dock-inner { display: flex; gap: 22px; padding: 12px 18px; border-radius: 26px; border: 1px solid rgba(255,255,255,0.13); background: rgba(22,28,42,0.42); backdrop-filter: blur(22px); box-shadow: 0 20px 50px rgba(0,0,0,0.4); pointer-events: auto; }
.dock-inner:empty { display: none; }
.dock .app { width: 76px; gap: 0; }
.dock .icon { width: 70px; height: 70px; border-radius: 18px; }
.dock .peek { bottom: calc(100% + 12px); }
.dock .label, .dock .sub { display: none; }
@media (max-width: 960px) {
  .content { grid-template-columns: 1fr; }
  .rail { position: static; }
}
@media (max-width: 640px) {
  main { padding: 14px 18px 120px; }
  .find input { width: 150px; }
  .icon { width: 74px; height: 74px; border-radius: 19px; }
  .grid { grid-template-columns: repeat(auto-fill, minmax(92px, 1fr)); gap: 24px 8px; }
  .app, .label { width: 92px; }
  .dock .icon { width: 58px; height: 58px; border-radius: 16px; }
  .peek { display: none; }
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
            dock: false,
            role: None,
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
        assert_eq!(
            parsed[1].upstream.as_ref().map(Upstream::all),
            Some(vec!["http://127.0.0.1:2", "https://b.example"])
        );
    }

    #[test]
    fn the_launcher_keeps_the_files_order_and_links_every_route() {
        let parsed = routes(
            r#"{"routes":[{"name":"store","hosts":["store.myc"],"upstream":"http://127.0.0.1:1"},{"name":"psst","hosts":["psst.myc"]}]}"#,
        );
        let page = page(
            &parsed,
            &StoreState::default(),
            &System::default(),
            None,
            None,
        );
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
                color: Some("#8bd5ca".into()),
                summary: Some("A shell in the browser".into()),
                category: Some("developer-tools".into()),
                version: Some("0.1.0".into()),
            }],
            ..StoreState::default()
        };
        let page = page(&parsed, &store, &System::default(), None, None);
        assert!(page.contains("--c:#8bd5ca"));
        assert!(page.contains(">terminal</span>"));
        assert!(page.contains("A shell in the browser"));
        assert!(page.contains("developer-tools"));
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(escape("<b> & \"q\""), "&lt;b&gt; &amp; &quot;q&quot;");
    }
}
