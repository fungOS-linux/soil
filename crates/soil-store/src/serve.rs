//! A loopback HTTP face for the storefront.
//!
//! `store.myc` in a browser needs HTML, not a Canvas scene, so this is the
//! smallest useful thing that can sit behind the Mycelium gateway: read the
//! catalog once, render it, answer three routes. It binds loopback only; the
//! gateway owns the name, the certificate and the outside world.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use soil_spore::manual::{Manual, read_prose};

use crate::index::{Entry, Index};
use crate::plan::{Installed, Plan};
use crate::snapshot::{Snapshot, Source, build};
use crate::{Error, Result};

pub struct Store {
    pub installed: Vec<Installed>,
    pub plan: Plan,
    pub snapshot: Snapshot,
    /// What the source offers, whether or not this host has it.
    pub catalog: BTreeMap<String, Entry>,
    /// The catalog this presenter reads. Kept so `/api/index` serves the bytes
    /// the rows came from instead of re-deriving them.
    pub index: Option<Index>,
}

impl Store {
    pub fn new(index: Option<&Index>, installed: Vec<Installed>, source: Source) -> Self {
        let catalog = index.map(Index::catalog).unwrap_or_default();
        let plan = crate::plan(&installed, &catalog);
        let snapshot = build(&installed, &plan, &catalog, &source);
        Self {
            installed,
            plan,
            snapshot,
            catalog,
            index: index.cloned(),
        }
    }

    fn find(&self, id: &str) -> Option<&Installed> {
        self.installed.iter().find(|app| app.id() == id)
    }

    pub fn page(&self) -> String {
        format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Spore Store</title><style>{css}</style></head><body>{header}<main><section class="grid">{cards}</section><section><h2>Updates</h2><ul class="updates">{updates}</ul></section></main>{footer}</body></html>"#,
            css = CSS,
            header = self.header(),
            cards = self.cards(),
            updates = self.updates(),
            footer = FOOTER,
        )
    }

    pub fn updates_page(&self) -> String {
        format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Updates · Spore Store</title><style>{css}</style></head><body>{header}<main><section><h2>Updates</h2><ul class="updates">{updates}</ul></section><section><h2>What the classes mean</h2><ul class="classes"><li><span class="chip live">live</span> reloads without interrupting work</li><li><span class="chip restart">restart</span> applies the next time the Spore starts</li><li><span class="chip consent">consent</span> waits for you to approve new grants</li><li><span class="chip system">system</span> changes the host, so it needs a session restart</li><li><span class="chip hold">hold</span> pinned by policy; the store will not offer it</li></ul></section></main>{footer}</body></html>"#,
            css = CSS,
            header = self.header(),
            updates = self.updates(),
            footer = FOOTER,
        )
    }

    fn header(&self) -> String {
        let source = &self.snapshot.value["source"];
        let pending = &self.snapshot.value["pending"];
        format!(
            r#"<header><div><h1><a href="/">Spore Store</a></h1><p class="subtle">{source} · {count} Spores · {verified}</p></div><div class="counts"><a href="/updates">{updates} updates</a><span>{consent} need consent</span><span>{reboot} need reboot</span></div></header>"#,
            source = escape(source["name"].as_str().unwrap_or("local")),
            count = self.snapshot.value["apps"].as_array().map_or(0, Vec::len),
            verified = if source["verified"].as_bool().unwrap_or(false) {
                "verified index"
            } else {
                "no signed index"
            },
            updates = pending["total"].as_u64().unwrap_or(0),
            consent = pending["consent"].as_u64().unwrap_or(0),
            reboot = pending["reboot"].as_u64().unwrap_or(0),
        )
    }

    fn cards(&self) -> String {
        let apps = self.snapshot.value["apps"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut cards = String::new();
        for app in &apps {
            let id = app["id"].as_str().unwrap_or_default();
            let state = app["state"].as_str().unwrap_or("get");
            cards.push_str(&format!(
                r#"<a class="card" href="/spore/{id}"><div class="mark">{badge}</div><div class="card-body"><div class="name">{name}<span class="version">{version}</span></div><div class="publisher">{publisher}</div><div class="summary">{summary}</div><div class="row"><span class="chip {state}">{state_label}</span><span class="category">{category}</span></div></div></a>"#,
                id = escape(id),
                badge = escape(app["badge"].as_str().unwrap_or("?")),
                name = escape(app["name"].as_str().unwrap_or(id)),
                version = escape(app["version"].as_str().unwrap_or_default()),
                publisher = escape(app["publisher"].as_str().unwrap_or("unknown")),
                summary = escape(app["summary"].as_str().unwrap_or("")),
                state = escape(state),
                state_label = match state {
                    "update" => "update ready",
                    "installed" => "installed",
                    _ => "available",
                },
                category = escape(app["category"].as_str().unwrap_or("")),
            ));
        }
        cards
    }

    fn updates(&self) -> String {
        let mut updates = String::new();
        for update in self.plan.updates.iter() {
            updates.push_str(&format!(
                r#"<li><span class="name">{id}</span><span class="from">{from} → {to}</span><span class="chip {class}">{class}</span><span class="reason">{reason}</span></li>"#,
                id = escape(&update.spore_id),
                from = escape(&update.from),
                to = escape(&update.to),
                class = update.class.as_str(),
                reason = escape(&update.reason),
            ));
        }
        if updates.is_empty() {
            updates.push_str(if self.installed.is_empty() {
                "<li class=\"empty\">Nothing is installed on this host yet, so there is nothing to update. Every Spore above is available to install.</li>"
            } else {
                "<li class=\"empty\">Every installed Spore matches the active source.</li>"
            });
        }
        updates
    }

    pub fn spore_page(&self, id: &str) -> Option<String> {
        let apps = self.snapshot.value["apps"].as_array()?;
        let row = apps.iter().find(|app| app["id"].as_str() == Some(id))?;
        let installed = self.find(id);
        let entry = self.catalog.get(id);
        if installed.is_none() && entry.is_none() {
            return None;
        }

        let (name, version, publisher, license, description, grants, manual) = match installed {
            Some(app) => {
                let manifest = &app.manifest;
                let prose = read_prose(&app.root, manifest).ok().flatten();
                let grants = manifest
                    .grants
                    .iter()
                    .map(|(grant, why)| {
                        format!("<li><code>{}</code> {}</li>", escape(grant), escape(why))
                    })
                    .collect::<String>();
                (
                    manifest.spore.name.clone(),
                    manifest.spore.version.clone(),
                    app.publisher.clone().unwrap_or_else(|| "unknown".into()),
                    manifest.spore.license.clone(),
                    manifest
                        .spore
                        .description
                        .clone()
                        .unwrap_or_else(|| manifest.spore.summary.clone()),
                    grants,
                    escape(&Manual::from_manifest(manifest, prose).render(80)),
                )
            }
            None => {
                let entry = entry?;
                let grants = if entry.grants.is_empty() {
                    "<li>No host capabilities requested.</li>".to_owned()
                } else {
                    entry
                        .grants
                        .iter()
                        .map(|grant| {
                            format!(
                                "<li><code>{}</code> declared in the catalog, reviewed at install</li>",
                                escape(grant)
                            )
                        })
                        .collect::<String>()
                };
                (
                    row["name"].as_str().unwrap_or(id).to_owned(),
                    entry.version.clone(),
                    entry.publisher.clone().unwrap_or_else(|| "unknown".into()),
                    "—".to_owned(),
                    format!(
                        "Available to install. Version {} is listed by the active source; nothing is installed on this host yet.",
                        entry.version
                    ),
                    grants,
                    escape(
                        "The manual ships inside the package. It appears here once the host has installed it.",
                    ),
                )
            }
        };

        let update = match self
            .plan
            .updates
            .iter()
            .find(|update| update.spore_id == id)
        {
            Some(update) => format!(
                "<p class=\"chip {}\">{} → {} · {}</p>",
                update.class.as_str(),
                escape(&update.from),
                escape(&update.to),
                escape(&update.reason)
            ),
            None if installed.is_some() => {
                "<p class=\"chip installed\">up to date with the active source</p>".to_owned()
            }
            None => "<p class=\"chip get\">not installed on this host</p>".to_owned(),
        };

        Some(format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{name} · Spore Store</title><style>{css}</style></head><body><header><div><p><a href="/">← Store</a></p><h1>{name} <span class="version">{version}</span></h1><p class="subtle">{publisher} · {license} · {id}</p></div></header><main><section><p class="summary">{description}</p>{update}<h2>What it can reach</h2><ul class="grants">{grants}</ul></section><section><h2>Manual</h2><pre class="manual">{manual}</pre></section></main><footer>Rendered from the package manifest and the active catalog; the manual ships inside the package.</footer></body></html>"#,
            css = CSS,
            name = escape(&name),
            version = escape(&version),
            publisher = escape(&publisher),
            license = escape(&license),
            id = escape(id),
            description = escape(&description),
            update = update,
            grants = grants,
            manual = manual,
        ))
    }
}

/// The manual is read from the package root the scan used.
pub fn serve(listener: TcpListener, store: Store) -> Result<()> {
    let store = std::sync::Arc::new(store);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let store = std::sync::Arc::clone(&store);
        std::thread::spawn(move || {
            let _ = answer(stream, &store);
        });
    }
    Ok(())
}

fn answer(mut stream: TcpStream, store: &Store) -> std::io::Result<()> {
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
            // An empty line ends the headers; waiting for EOF would hang the
            // request until the client gave up.
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let (status, body, content_type) = match (method, path) {
        ("GET", "/") => ("200 OK", store.page(), HTML),
        ("GET", "/updates") => ("200 OK", store.updates_page(), HTML),
        ("GET", "/healthz") => ("200 OK", "ok\n".into(), TEXT),
        ("GET", "/api/index") => match &store.index {
            Some(index) => (
                "200 OK",
                serde_json::to_string_pretty(index).unwrap_or_else(|_| "{}".into()),
                JSON,
            ),
            None => ("404 Not Found", "no index\n".into(), TEXT),
        },
        ("GET", "/api/apps") => {
            let value = &store.snapshot.value;
            let body = serde_json::json!({
                "apps": value["apps"],
                "pending": value["pending"],
                "source": value["source"],
            });
            ("200 OK", body.to_string(), JSON)
        }
        ("GET", "/api/plan") => (
            "200 OK",
            serde_json::json!({
                "updates": store.plan.updates.iter().map(|u| serde_json::json!({
                    "id": u.spore_id, "from": u.from, "to": u.to,
                    "class": u.class.as_str(), "reason": u.reason,
                })).collect::<Vec<_>>(),
                "current": store.plan.current,
                "yanked": store.plan.yanked,
            })
            .to_string(),
            JSON,
        ),
        ("GET", other) if other.starts_with("/spore/") => {
            let id = &other[7..];
            match store.spore_page(id) {
                Some(page) => ("200 OK", page, HTML),
                None => ("404 Not Found", "no such Spore\n".into(), TEXT),
            }
        }
        _ => ("404 Not Found", "not found\n".into(), TEXT),
    };

    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

const HTML: &str = "text/html; charset=utf-8";
const JSON: &str = "application/json";
const TEXT: &str = "text/plain; charset=utf-8";

pub fn bind(address: &str) -> Result<TcpListener> {
    TcpListener::bind(address).map_err(|error| Error::Io(format!("listen {address}: {error}")))
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

const CSS: &str = r#"
:root { color-scheme: dark; }
* { box-sizing: border-box; }
body { margin: 0; background: #0f1115; color: #e7e9ee; font: 15px/1.5 ui-sans-serif, -apple-system, system-ui, sans-serif; }
header { display: flex; justify-content: space-between; align-items: end; gap: 24px; padding: 28px 32px 20px; border-bottom: 1px solid #232733; }
h1 { margin: 0; font-size: 26px; font-weight: 600; letter-spacing: -0.02em; }
h2 { margin: 0 0 12px; font-size: 15px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.08em; color: #9aa3b2; }
a { color: inherit; text-decoration: none; }
main { padding: 24px 32px 48px; display: grid; gap: 32px; }
.subtle { margin: 4px 0 0; color: #9aa3b2; font-size: 13px; }
.counts { display: flex; gap: 16px; color: #9aa3b2; font-size: 13px; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 14px; }
.card { display: flex; gap: 14px; padding: 16px; border: 1px solid #232733; border-radius: 12px; background: #151922; }
.card:hover { border-color: #3a4160; }
.mark { width: 44px; height: 44px; border-radius: 10px; background: #232733; display: grid; place-items: center; font-size: 20px; font-weight: 600; color: #8b9dff; }
.card-body { min-width: 0; }
.name { font-weight: 600; }
.version { margin-left: 8px; color: #9aa3b2; font-weight: 400; font-size: 12px; }
.publisher, .category { color: #9aa3b2; font-size: 12px; }
.summary { margin: 4px 0 10px; color: #c3c9d4; font-size: 13px; }
.row { display: flex; justify-content: space-between; gap: 8px; align-items: center; }
.chip { display: inline-block; padding: 2px 8px; border-radius: 999px; font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; }
.chip.installed { background: #1d2a22; color: #7ad0a3; }
.chip.update { background: #2a2618; color: #f2c14e; }
.chip.get { background: #1a2130; color: #8b9dff; }
.chip.live { background: #1d2a22; color: #7ad0a3; }
.chip.restart { background: #1a2130; color: #8b9dff; }
.chip.consent { background: #2a2618; color: #f2c14e; }
.chip.system { background: #2d1a1c; color: #f08a8a; }
.chip.hold { background: #232733; color: #9aa3b2; }
.updates { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
.updates li { display: grid; grid-template-columns: 220px 140px 100px 1fr; gap: 12px; align-items: center; padding: 10px 14px; border: 1px solid #232733; border-radius: 10px; background: #141821; font-size: 13px; }
.updates .from { color: #9aa3b2; }
.updates .reason { color: #c3c9d4; }
.updates .empty { display: block; color: #9aa3b2; }
.grants { margin: 0; padding-left: 18px; color: #c3c9d4; }
.classes { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; color: #9aa3b2; font-size: 13px; }
.classes li { display: flex; gap: 10px; align-items: center; }
.classes .chip { min-width: 76px; text-align: center; }
pre.manual { margin: 0; padding: 20px; border: 1px solid #232733; border-radius: 12px; background: #12151c; color: #c3c9d4; font: 12.5px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; overflow-x: auto; }
footer { padding: 16px 32px 28px; color: #6f7787; font-size: 12px; border-top: 1px solid #232733; }
"#;

const FOOTER: &str = "<footer>Served on loopback by <code>soil-store serve</code> · the index is signed and package digests are verified at install</footer>";
