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
    /// Where packed tars live, named `<digest>.spore` — the same layout
    /// `soil-install` keeps. A store with no directory serves listings only.
    pub packages: Option<std::path::PathBuf>,
    /// Where the bytes behind an `ipfs://` link live, named by their CID.
    pub media: Option<std::path::PathBuf>,
    /// The operator's review file, read once at startup.
    pub reviews: Option<crate::Reviews>,
    /// A Spore's own storefront page, served instead of the built-in one.
    pub page_html: Option<String>,
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
            packages: None,
            media: None,
            reviews: None,
            page_html: None,
        }
    }

    /// Serve packed tars from `dir`. Only digests the catalog offers are
    /// readable, so the directory cannot be used to enumerate the disk.
    pub fn with_packages(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.packages = Some(dir.into());
        self
    }

    /// Serve artwork and listings from `dir`, keyed by CID.
    pub fn with_media(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.media = Some(dir.into());
        self
    }

    /// Serve the operator's reviews.
    pub fn with_reviews(mut self, reviews: crate::Reviews) -> Self {
        self.reviews = Some(reviews);
        self
    }

    /// Serve a Spore's own storefront page at `/` instead of the built-in one.
    ///
    /// The page is the package's `web` surface: a store is a Spore, so the page
    /// it ships is the storefront, and this presenter is only the origin that
    /// hands it out. `dir` is the surface's directory, `web/` in the package.
    pub fn with_page(mut self, dir: impl AsRef<std::path::Path>) -> Self {
        let index = dir.as_ref().join("index.html");
        self.page_html = std::fs::read_to_string(&index).ok();
        if self.page_html.is_none() {
            eprintln!(
                "soil-store: no page at {}, serving the built-in storefront",
                index.display()
            );
        }
        self
    }

    /// The bytes an `ipfs://` address names, when this store keeps them.
    pub fn blob(&self, address: &str) -> Option<Vec<u8>> {
        crate::media::get(self.media.as_ref()?, address)
    }

    /// The listing a catalog entry points at, if this store has the bytes.
    pub fn listing(&self, entry: &Entry) -> Option<crate::Listing> {
        let bytes = self.blob(entry.listing.as_ref()?)?;
        crate::Listing::parse(std::str::from_utf8(&bytes).ok()?).ok()
    }

    /// `/media/<cid>` in a page: the address without its scheme.
    fn media_path(address: &str) -> &str {
        address.strip_prefix("ipfs://").unwrap_or(address)
    }

    /// One catalog row, as an agent reads it.
    pub fn row(&self, entry: &Entry) -> serde_json::Value {
        let ratings = self
            .reviews
            .as_ref()
            .map(|reviews| reviews.aggregate(&entry.digest, &entry.spore_id));
        serde_json::json!({
            "id": entry.spore_id,
            "version": entry.version,
            "digest": entry.digest,
            "package": entry.package,
            "signature": format!("/signature/{}", entry.digest),
            "publisher": entry.publisher,
            "publisher_key": entry.publisher_key,
            "system": entry.system,
            "grants": entry.grants,
            "listing": entry.listing,
            "document": self.listing(entry),
            "ratings": ratings,
            "url": format!("/s/{}", entry.spore_id),
        })
    }

    /// The catalog rows matching a query, over the same text the page shows.
    ///
    /// The search belongs to the store, not to a client: a Spore view, a
    /// browser and an agent have to agree about what matches a word, and the
    /// only way that survives a second client is one implementation of the
    /// answer. Every word must match; a category is a filter, not a query.
    pub fn search(&self, query: &str, category: Option<&str>) -> Vec<&Entry> {
        let needle = query.trim().to_lowercase();
        let mut rows: Vec<&Entry> = self
            .catalog
            .values()
            .filter(|entry| {
                let listing = self.listing(entry);
                if let Some(category) = category {
                    let named = listing.as_ref().is_some_and(|listing| {
                        listing.categories.iter().any(|value| value == category)
                    });
                    if !named {
                        return false;
                    }
                }
                if needle.is_empty() {
                    return true;
                }
                let listing = listing.as_ref();
                let mut fields = vec![
                    entry.spore_id.to_lowercase(),
                    entry.version.to_lowercase(),
                    entry.publisher.clone().unwrap_or_default().to_lowercase(),
                    entry.grants.join(" ").to_lowercase(),
                ];
                if let Some(listing) = listing {
                    fields.push(listing.name.to_lowercase());
                    fields.push(listing.summary.to_lowercase());
                    fields.push(
                        listing
                            .description
                            .clone()
                            .unwrap_or_default()
                            .to_lowercase(),
                    );
                    fields.push(listing.categories.join(" ").to_lowercase());
                    fields.push(listing.keywords.join(" ").to_lowercase());
                }
                let haystack = fields.join(" ");
                needle
                    .split_whitespace()
                    .all(|word| haystack.contains(word))
            })
            .collect();
        rows.sort_by(|a, b| a.spore_id.cmp(&b.spore_id));
        rows
    }

    /// The tar for a digest the active catalog lists, if this store has it.
    pub fn package(&self, digest: &str) -> Option<Vec<u8>> {
        self.artifact(digest, "spore")
    }

    /// The publisher's detached signature over that tar, so a client can check
    /// the bytes against the key the index delegated without trusting this host.
    pub fn signature(&self, digest: &str) -> Option<Vec<u8>> {
        self.artifact(digest, "spore.asc")
    }

    fn artifact(&self, digest: &str, extension: &str) -> Option<Vec<u8>> {
        let dir = self.packages.as_ref()?;
        if !crate::digest(digest) || !self.catalog.values().any(|entry| entry.digest == digest) {
            return None;
        }
        std::fs::read(dir.join(format!("{digest}.{extension}"))).ok()
    }

    fn find(&self, id: &str) -> Option<&Installed> {
        self.installed.iter().find(|app| app.id() == id)
    }

    pub fn page(&self) -> String {
        if let Some(page) = &self.page_html {
            return page.clone();
        }
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
            // Artwork is enrichment: the monogram is what a launcher draws when
            // nothing resolves, so a card is never blank while a CID is missing.
            let listing = self.catalog.get(id).and_then(|entry| self.listing(entry));
            let mark = match listing.as_ref().and_then(|value| value.icon.as_deref()) {
                Some(icon) if self.blob(icon).is_some() => format!(
                    r#"<img class="mark" src="/media/{}" alt="">"#,
                    escape(Self::media_path(icon))
                ),
                _ => format!(
                    r#"<div class="mark">{}</div>"#,
                    escape(app["badge"].as_str().unwrap_or("?"))
                ),
            };
            cards.push_str(&format!(
                r#"<a class="card" href="/spore/{id}">{mark}<div class="card-body"><div class="name">{name}<span class="version">{version}</span></div><div class="publisher">{publisher}</div><div class="summary">{summary}</div><div class="row"><span class="chip {state}">{state_label}</span><span class="category">{category}</span></div></div></a>"#,
                id = escape(id),
                mark = mark,
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

        // Screenshots are the listing's, not the package's: a client draws the
        // ones it can fetch and skips the rest rather than showing a hole.
        let gallery = entry
            .and_then(|entry| self.listing(entry))
            .map(|listing| {
                listing
                    .screenshots
                    .iter()
                    .filter(|shot| self.blob(&shot.cid).is_some())
                    .map(|shot| {
                        format!(
                            r#"<figure><img src="/media/{src}" alt="{alt}"><figcaption>{alt}</figcaption></figure>"#,
                            src = escape(Self::media_path(&shot.cid)),
                            alt = escape(&shot.caption),
                        )
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        let gallery = if gallery.is_empty() {
            String::new()
        } else {
            format!("<section><h2>Screenshots</h2><div class=\"shots\">{gallery}</div></section>")
        };

        // A rating is a claim about a digest, so the heading says which one:
        // "4.6 of this build" and "4.2 across all versions" are two facts.
        let review_section = match (entry, self.reviews.as_ref()) {
            (Some(entry), Some(reviews)) => {
                let aggregate = reviews.aggregate(&entry.digest, &entry.spore_id);
                let mut items = String::new();
                for review in reviews.visible(&entry.digest) {
                    items.push_str(&format!(
                        r#"<li><span class="stars">{}</span><strong>{}</strong><p>{}</p></li>"#,
                        "★".repeat(usize::from(review.rating)),
                        escape(&review.principal),
                        escape(&review.body),
                    ));
                }
                if items.is_empty() {
                    "<section><h2>Reviews</h2><p class=\"subtle\">No reviews of this build yet.</p></section>".to_owned()
                } else {
                    format!(
                        "<section><h2>Reviews <span class=\"subtle\">★ {weighted:.1} · {count} of this build · {all:.1} across all versions ({total})</span></h2><ul class=\"reviews\">{items}</ul></section>",
                        weighted = aggregate.current.weighted,
                        count = aggregate.current.count,
                        all = aggregate.all.weighted,
                        total = aggregate.all.count,
                    )
                }
            }
            _ => String::new(),
        };

        Some(format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{name} · Spore Store</title><style>{css}</style></head><body><header><div><p><a href="/">← Store</a></p><h1>{name} <span class="version">{version}</span></h1><p class="subtle">{publisher} · {license} · {id}</p></div></header><main><section><p class="summary">{description}</p>{update}{gallery}{reviews}<h2>What it can reach</h2><ul class="grants">{grants}</ul></section><section><h2>Manual</h2><pre class="manual">{manual}</pre></section></main><footer>Rendered from the package manifest and the active catalog; the manual ships inside the package.</footer></body></html>"#,
            css = CSS,
            name = escape(&name),
            version = escape(&version),
            publisher = escape(&publisher),
            license = escape(&license),
            id = escape(id),
            description = escape(&description),
            update = update,
            gallery = gallery,
            reviews = review_section,
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
    // Shareable links carry the query: `?q=` is the store's search, not the
    // client's, so the same URL opens the same results in a browser and a Spore.
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    };
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

    let (status, body, content_type): (&str, Vec<u8>, &str) = match (method, path) {
        ("GET", "/") => ("200 OK", store.page().into_bytes(), HTML),
        ("GET", "/updates") => ("200 OK", store.updates_page().into_bytes(), HTML),
        ("GET", "/healthz") => ("200 OK", b"ok\n".to_vec(), TEXT),
        ("GET", "/api/index") => match &store.index {
            Some(index) => (
                "200 OK",
                serde_json::to_string_pretty(index)
                    .unwrap_or_else(|_| "{}".into())
                    .into_bytes(),
                JSON,
            ),
            None => ("404 Not Found", b"no index\n".to_vec(), TEXT),
        },
        ("GET", "/api/apps") => {
            let value = &store.snapshot.value;
            let body = serde_json::json!({
                "apps": value["apps"],
                "pending": value["pending"],
                "source": value["source"],
            });
            ("200 OK", body.to_string().into_bytes(), JSON)
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
            .to_string()
            .into_bytes(),
            JSON,
        ),
        ("GET", "/api/catalog") => {
            // The machine view: an agent gets the same rows a browser draws, with
            // each listing resolved to its document and the package it names.
            let rows: Vec<_> = store
                .catalog
                .values()
                .map(|entry| store.row(entry))
                .collect();
            (
                "200 OK",
                serde_json::json!({ "source": store.snapshot.value["source"], "entries": rows })
                    .to_string()
                    .into_bytes(),
                JSON,
            )
        }
        ("GET", "/api/reviews") => {
            let requested = param(query, "digest").unwrap_or_default();
            match (
                store.reviews.as_ref(),
                store
                    .catalog
                    .values()
                    .find(|entry| entry.digest == requested),
            ) {
                (Some(reviews), Some(entry)) => (
                    "200 OK",
                    serde_json::json!({
                        "digest": requested,
                        "spore_id": entry.spore_id,
                        "ratings": reviews.aggregate(&requested, &entry.spore_id),
                        "reviews": reviews.visible(&requested),
                    })
                    .to_string()
                    .into_bytes(),
                    JSON,
                ),
                (None, _) => ("404 Not Found", b"no reviews here\n".to_vec(), TEXT),
                (Some(_), None) => (
                    "404 Not Found",
                    b"no such package in this catalog\n".to_vec(),
                    TEXT,
                ),
            }
        }
        ("GET", "/api/search") => {
            let text = param(query, "q").unwrap_or_default();
            let category = param(query, "category");
            let rows: Vec<_> = store
                .search(&text, category.as_deref())
                .into_iter()
                .map(|entry| store.row(entry))
                .collect();
            let body = serde_json::json!({
                "query": text,
                "category": category,
                "total": rows.len(),
                "entries": rows,
            });
            ("200 OK", body.to_string().into_bytes(), JSON)
        }
        ("GET", other) if other.starts_with("/media/") => {
            let address = &other[7..];
            match store.blob(address) {
                Some(bytes) => {
                    let kind = image_type(&bytes);
                    ("200 OK", bytes, kind)
                }
                None => (
                    "404 Not Found",
                    b"no such content on this store\n".to_vec(),
                    TEXT,
                ),
            }
        }
        ("GET", other) if other.starts_with("/spore/") || other.starts_with("/s/") => {
            // A shareable link opens the Spore's own storefront when it ships
            // one: the page reads the path and routes itself. The built-in page
            // stays for a store with no Spore.
            if other.starts_with("/s/") && store.page_html.is_some() {
                ("200 OK", store.page().into_bytes(), HTML)
            } else {
                let id = if let Some(id) = other.strip_prefix("/spore/") {
                    id
                } else {
                    &other[3..]
                };
                match store.spore_page(id) {
                    Some(page) => ("200 OK", page.into_bytes(), HTML),
                    None => ("404 Not Found", b"no such Spore\n".to_vec(), TEXT),
                }
            }
        }
        ("GET", other) if other.starts_with("/package/") => {
            let digest = &other[9..];
            match store.package(digest) {
                Some(bytes) => ("200 OK", bytes, SPORE),
                None => (
                    "404 Not Found",
                    b"no such package on this store\n".to_vec(),
                    TEXT,
                ),
            }
        }
        ("GET", other) if other.starts_with("/signature/") => {
            let digest = &other[11..];
            match store.signature(digest) {
                Some(bytes) => ("200 OK", bytes, PGP),
                None => (
                    "404 Not Found",
                    b"no signature on this store\n".to_vec(),
                    TEXT,
                ),
            }
        }
        _ => ("404 Not Found", b"not found\n".to_vec(), TEXT),
    };

    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()
}

const HTML: &str = "text/html; charset=utf-8";
const JSON: &str = "application/json";
const TEXT: &str = "text/plain; charset=utf-8";
const SPORE: &str = "application/vnd.fungos.spore";
const PGP: &str = "application/pgp-signature";

/// One query parameter, percent-decoded.
fn param(query: Option<&str>, name: &str) -> Option<String> {
    let query = query?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| decode(value))
    })
}

/// `%20` and `+` mean a space in a query string; everything else stands.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What an `ipfs://` blob is, from its first bytes.
///
/// A CID carries no file name, so the store cannot trust an extension even if a
/// client sends one. The four formats a listing is allowed to use are cheap to
/// recognise; anything else is a blob to download, not to render.
fn image_type(bytes: &[u8]) -> &'static str {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.starts_with(PNG) {
        return "image/png";
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        return "image/svg+xml";
    }
    "application/octet-stream"
}

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
.mark { width: 44px; height: 44px; border-radius: 10px; background: #232733; display: grid; place-items: center; font-size: 20px; font-weight: 600; color: #8b9dff; object-fit: cover; flex: 0 0 44px; }
.shots { display: grid; gap: 14px; }
.shots figure { margin: 0; }
.shots img { width: 100%; border-radius: 12px; border: 1px solid #232733; display: block; }
.shots figcaption { margin-top: 6px; color: #9aa3b2; font-size: 12px; }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Checkpoint, EntryKind, SCHEMA};

    const CID: &str = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";

    fn store(packages: Option<&std::path::Path>) -> Store {
        let digest = "a".repeat(64);
        let entry = Entry {
            seq: 1,
            kind: EntryKind::Add,
            spore_id: "dev.fungos.demo".into(),
            version: "0.1.0".into(),
            digest: digest.clone(),
            package: CID.into(),
            listing: None,
            module: None,
            ui: None,
            grants: Vec::new(),
            system: None,
            publisher: None,
            publisher_key: None,
            signature: String::new(),
        };
        let index = Index {
            schema: SCHEMA,
            source: "test".into(),
            checkpoint: Checkpoint {
                seq: 1,
                root: Index::root(std::slice::from_ref(&entry)),
                signature: String::new(),
            },
            entries: vec![entry],
            delegations: Vec::new(),
            revocations: Vec::new(),
        };
        let source = Source {
            name: "test".into(),
            index: "unsigned".into(),
            verified: false,
        };
        let store = Store::new(Some(&index), Vec::new(), source);
        match packages {
            Some(dir) => store.with_packages(dir),
            None => store,
        }
    }

    #[test]
    fn only_catalogued_digests_are_served() {
        let temp = tempfile::tempdir().unwrap();
        let listed = "a".repeat(64);
        let unlisted = "b".repeat(64);
        std::fs::write(temp.path().join(format!("{listed}.spore")), b"tar").unwrap();
        std::fs::write(temp.path().join(format!("{listed}.spore.asc")), b"sig").unwrap();
        std::fs::write(temp.path().join(format!("{unlisted}.spore")), b"other").unwrap();
        std::fs::write(temp.path().join(format!("{unlisted}.spore.asc")), b"other").unwrap();

        let served = store(Some(temp.path()));
        assert_eq!(served.package(&listed), Some(b"tar".to_vec()));
        assert_eq!(served.signature(&listed), Some(b"sig".to_vec()));
        assert_eq!(
            served.package(&unlisted),
            None,
            "a digest the catalog omits"
        );
        assert_eq!(
            served.signature(&unlisted),
            None,
            "a signature for an unlisted digest"
        );
        assert_eq!(
            served.package("../../etc/passwd"),
            None,
            "a path is not a digest"
        );
        assert_eq!(
            store(None).package(&listed),
            None,
            "a listing-only store serves no bytes"
        );
    }

    /// A listing document, as a publisher would store it.
    fn listing(id: &str, summary: &str, categories: &[&str], keywords: &[&str]) -> crate::Listing {
        crate::Listing {
            schema: crate::listing::LISTING_SCHEMA.into(),
            spore_id: id.into(),
            version: "0.1.0".into(),
            digest: "a".repeat(64),
            name: id.into(),
            summary: summary.into(),
            description: None,
            license: "MIT".into(),
            homepage: None,
            categories: categories.iter().map(|value| (*value).to_owned()).collect(),
            keywords: keywords.iter().map(|value| (*value).to_owned()).collect(),
            icon: None,
            screenshots: Vec::new(),
            badge: None,
            color: None,
            system: None,
            grants: Vec::new(),
        }
    }

    fn store_with(entries: Vec<Entry>, media: &std::path::Path) -> Store {
        let index = Index {
            schema: SCHEMA,
            source: "test".into(),
            checkpoint: Checkpoint {
                seq: entries.len() as u64,
                root: Index::root(&entries),
                signature: String::new(),
            },
            entries,
            delegations: Vec::new(),
            revocations: Vec::new(),
        };
        let source = Source {
            name: "test".into(),
            index: "unsigned".into(),
            verified: false,
        };
        Store::new(Some(&index), Vec::new(), source).with_media(media)
    }

    #[test]
    fn search_matches_prose_and_filters_by_category() {
        let media = tempfile::tempdir().unwrap();
        let mut entries = Vec::new();
        for (id, summary, categories, keywords) in [
            (
                "dev.fungos.editor",
                "A markdown editor with a live preview",
                &["productivity"][..],
                &["markdown", "writing"][..],
            ),
            (
                "dev.fungos.graph",
                "Plot a dataframe without leaving the shell",
                &["science"][..],
                &["charts", "dataframe"][..],
            ),
        ] {
            let document = listing(id, summary, categories, keywords);
            let bytes = serde_json::to_vec(&document).unwrap();
            let address = crate::media::add(media.path(), &bytes).unwrap();
            entries.push(Entry {
                seq: entries.len() as u64 + 1,
                kind: EntryKind::Add,
                spore_id: id.into(),
                version: "0.1.0".into(),
                digest: "a".repeat(64),
                package: CID.into(),
                listing: Some(address),
                module: None,
                ui: None,
                grants: Vec::new(),
                system: None,
                publisher: None,
                publisher_key: None,
                signature: String::new(),
            });
        }
        let store = store_with(entries, media.path());

        let ids = |rows: Vec<&Entry>| {
            rows.iter()
                .map(|row| row.spore_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(store.search("markdown", None)),
            vec!["dev.fungos.editor"],
            "a keyword in the listing is searchable"
        );
        assert_eq!(
            ids(store.search("plot dataframe", None)),
            vec!["dev.fungos.graph"],
            "every word has to match"
        );
        assert_eq!(
            ids(store.search("shell", None)),
            vec!["dev.fungos.graph"],
            "the summary is part of the text"
        );
        assert_eq!(
            ids(store.search("", Some("science"))),
            vec!["dev.fungos.graph"],
            "a category filters without a query"
        );
        assert!(store.search("markdown", Some("science")).is_empty());
        assert_eq!(
            store.search("", None).len(),
            2,
            "no query is the whole catalog"
        );
    }
}
