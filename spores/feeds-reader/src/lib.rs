//! Feed scraper/reader Spore.
//!
//! HTTP transport is host-mediated by `soil:http`: the host owns the origin
//! allowlist, budgets, TLS, routing, and credential material. The Spore owns
//! feed URLs, parsing, and the records projection. Settings come from
//! `soil:appdata`; items land in `soil:records`.

wit_bindgen::generate!({ path: "wit", world: "feeds-reader", generate_all });

use serde::{Deserialize, Serialize};
use soil::appdata::appdata::{self, Scope};
use soil::http::http::{self, Header, Method, Request};
use soil::records::records::{self, Entry, Key};

const CONFIG_KEY: &str = "feeds/config.json";
const DEFAULT_COLLECTION: &str = "feed";
const ITEM_SCHEMA: &str = "fungos.feeds/item";
const MAX_FEED_BYTES: u32 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Config {
    collection: String,
    limit: u32,
    title: String,
    feeds: Vec<FeedConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeedConfig {
    url: String,
    #[serde(default)]
    credential: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            collection: DEFAULT_COLLECTION.into(),
            limit: 100,
            title: "Feeds".into(),
            feeds: Vec::new(),
        }
    }
}

struct Component;

export!(Component);

impl Guest for Component {
    fn run(_args: Vec<String>) -> Result<(), String> {
        let store = appdata::open(Scope::Synced).map_err(|error| format!("{error:?}"))?;
        let config = match store.get(CONFIG_KEY).map_err(|error| format!("{error:?}"))? {
            Some(bytes) => serde_json::from_slice::<Config>(&bytes)
                .map_err(|error| format!("config: {error}"))?,
            None => {
                let config = Config::default();
                let bytes = serde_json::to_vec_pretty(&config)
                    .map_err(|error| format!("config: {error}"))?;
                store
                    .set(CONFIG_KEY, &bytes)
                    .map_err(|error| format!("write default config: {error:?}"))?;
                println!("seeded {CONFIG_KEY}; add feed URLs to this AppData key");
                config
            }
        };

        if config.feeds.is_empty() {
            println!("{} · no feeds configured", config.title);
            return Ok(());
        }

        for feed in &config.feeds {
            let count = match fetch_feed(feed, &config.collection, config.limit) {
                Ok(count) => count,
                Err(error) => {
                    println!("{}: {error}", feed.url);
                    continue;
                }
            };
            println!("{}: {count} items", feed.url);
        }
        Ok(())
    }
}

fn fetch_feed(feed: &FeedConfig, collection: &str, limit: u32) -> Result<usize, String> {
    let response = http::fetch(&Request {
        url: feed.url.clone(),
        method: Method::Get,
        headers: vec![Header {
            name: "accept".into(),
            value: "application/rss+xml, application/atom+xml, application/xml, text/xml, */*".into(),
        }],
        credential: feed.credential.clone(),
        max_bytes: MAX_FEED_BYTES,
        timeout_ms: 20_000,
    })
    .map_err(|error| format!("http {error:?}"))?;
    if response.status >= 400 {
        return Err(format!("http status {}", response.status));
    }
    let parsed = feed_rs::parser::parse(response.body.as_slice())
        .map_err(|error| format!("parse: {error}"))?;
    let feed_title = parsed.title.as_ref().map(|title| title.content.clone());
    let mut count = 0;
    for entry in parsed.entries.into_iter().take(limit.max(1) as usize) {
        let link = entry.links.first().map(|link| link.href.clone());
        let uid = if entry.id.trim().is_empty() {
            link.clone().unwrap_or_default()
        } else {
            entry.id.clone()
        };
        if uid.is_empty() {
            continue;
        }
        let id = format!(
            "feed-{}",
            blake3::hash(format!("{}\0{uid}", feed.url).as_bytes()).to_hex()
        );
        let authors: Vec<String> = entry
            .authors
            .iter()
            .filter_map(|person| person.name.clone().or_else(|| person.email.clone()))
            .collect();
        let categories: Vec<String> = entry
            .categories
            .iter()
            .map(|category| category.term.clone())
            .filter(|term| !term.is_empty())
            .collect();
        let body = serde_json::json!({
            "feed_url": feed.url,
            "feed_title": feed_title,
            "title": entry.title.as_ref().map(|title| title.content.clone()).unwrap_or_default(),
            "link": link,
            "summary": entry.summary.as_ref().map(|summary| summary.content.clone()).unwrap_or_default(),
            "content": entry.content.as_ref().and_then(|content| content.body.clone()).unwrap_or_default(),
            "authors": authors,
            "categories": categories,
            "published": entry.published.map(|date| date.to_rfc3339()),
            "updated": entry.updated.map(|date| date.to_rfc3339()),
            "uid": uid,
        });
        let body = serde_json::to_vec(&body).map_err(|error| format!("encode: {error}"))?;
        records::upsert(&Entry {
            key: Key {
                collection: collection.to_string(),
                id,
            },
            schema: ITEM_SCHEMA.into(),
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body,
        })
        .map_err(|error| format!("records {error:?}"))?;
        count += 1;
    }
    Ok(count)
}
