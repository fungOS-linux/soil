//! Feed reader Spore spike.
//!
//! This Spore does not fetch the network. A host-side poller writes feed items
//! into `soil:records`; the Spore owns the reader policy and its private
//! settings in `soil:appdata`. The config key is Spore-scoped by the host, so
//! no app id or dotfile path is supplied here.

wit_bindgen::generate!({ path: "wit", world: "feeds-reader", generate_all });

use serde::{Deserialize, Serialize};
use soil::appdata::appdata::{self, Scope};
use soil::records::records;

const CONFIG_KEY: &str = "feeds/config.json";
const DEFAULT_COLLECTION: &str = "feed";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Config {
    /// Host-mapped records collection. The host still owns the book.
    collection: String,
    limit: u32,
    /// Reserved for a future UI; currently documentation for the reader.
    title: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            collection: DEFAULT_COLLECTION.into(),
            limit: 50,
            title: "Feeds".into(),
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
                println!("seeded {CONFIG_KEY}: {config:?}");
                config
            }
        };

        let page = records::scan(&config.collection, None, config.limit.max(1).min(256))
            .map_err(|error| format!("{error:?}"))?;
        println!("{} · collection={}", config.title, config.collection);
        if page.entries.is_empty() {
            println!("no items yet; run `records import-rss` on the host");
        }
        for entry in page.entries {
            let body: serde_json::Value =
                serde_json::from_slice(&entry.body).unwrap_or(serde_json::Value::Null);
            let title = body["title"].as_str().unwrap_or(&entry.key.id);
            let published = body["published"].as_str().unwrap_or("");
            let link = body["link"].as_str().unwrap_or("");
            println!("{published}\t{title}\t{link}");
        }
        Ok(())
    }
}
