//! The storefront snapshot.
//!
//! The view is a pure function of this table: `spores/store/ui/store.lua` reads
//! it and draws. Keeping the shape in one place is what lets the tests assert
//! on what a person sees without a Canvas in the loop.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use soil_spore::CATEGORIES;

use crate::index::Entry;
use crate::plan::{Class, Installed, Plan};

/// The source a snapshot was built from.
#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub index: String,
    pub verified: bool,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub value: Value,
}

impl Snapshot {
    /// The Lua file a composer embeds: `return { ... }`.
    pub fn to_lua(&self) -> String {
        format!("return {}\n", to_lua(&self.value))
    }
}

pub fn build(
    installed: &[Installed],
    plan: &Plan,
    catalog: &BTreeMap<String, Entry>,
    source: &Source,
) -> Snapshot {
    let mut by_class: BTreeMap<&str, Class> = BTreeMap::new();
    for update in &plan.updates {
        by_class.insert(update.spore_id.as_str(), update.class);
    }

    let mut apps: Vec<Value> = installed
        .iter()
        .map(|app| {
            let id = app.id();
            let state = match by_class.get(id) {
                Some(_) => "update",
                None => "installed",
            };
            app_json(app, catalog.get(id), state)
        })
        .collect();

    // What the source offers and this machine does not have yet. Display data
    // lives in the listing CID, which the service fetches later; until then the
    // row carries identity only, which is the honest minimum.
    let known: BTreeSet<&str> = installed.iter().map(|app| app.id()).collect();
    for (id, entry) in catalog {
        if known.contains(id.as_str()) {
            continue;
        }
        apps.push(json!({
            "id": id,
            "name": short_name(id),
            "publisher": entry.publisher.clone().unwrap_or_else(|| "unknown".into()),
            "category": "utilities",
            "version": entry.version,
            "badge": "?",
            "state": "get",
            "summary": "",
            "license": "",
            "digest": entry.digest,
        }));
    }

    apps.sort_by(|a, b| {
        let left = a["name"].as_str().unwrap_or_default();
        let right = b["name"].as_str().unwrap_or_default();
        left.cmp(right)
    });

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for app in &apps {
        if let Some(category) = app["category"].as_str() {
            *counts.entry(category.to_owned()).or_default() += 1;
        }
    }

    let installed_count = installed.len();
    let mut categories = vec![
        json!({ "id": "all", "label": "All Spores", "count": apps.len(), "selected": true }),
        json!({ "id": "updates", "label": "Updates", "count": plan.updates.len() }),
        json!({ "id": "installed", "label": "Installed", "count": installed_count }),
    ];
    for category in CATEGORIES {
        categories.push(json!({
            "id": category,
            "label": label(category),
            "count": counts.get(category).copied().unwrap_or(0),
        }));
    }

    let updates: Vec<Value> = plan
        .updates
        .iter()
        .map(|update| {
            json!({
                "id": update.spore_id,
                "name": display_name(installed, &update.spore_id),
                "from": update.from,
                "to": update.to,
                "class": update.class.as_str(),
                "blurb": update.class.blurb(),
                "reason": update.reason,
                "digest": update.digest,
            })
        })
        .collect();

    let value = json!({
        "route": "browse",
        "source": {
            "name": source.name,
            "index": source.index,
            "verified": source.verified,
        },
        "query": "",
        "category": "all",
        "categories": categories,
        "apps": apps,
        "updates": updates,
        "pending": {
            "total": plan.updates.len(),
            "consent": plan.count(Class::Consent),
            "reboot": plan.count(Class::System),
            "hold": plan.count(Class::Hold),
        },
    });
    Snapshot { value }
}

fn app_json(app: &Installed, entry: Option<&Entry>, state: &str) -> Value {
    let manifest = &app.manifest;
    let presentation = manifest.presentation.as_ref();
    json!({
        "id": manifest.spore.id,
        "name": manifest.spore.name,
        "publisher": entry
            .and_then(|entry| entry.publisher.clone())
            .or_else(|| app.publisher.clone())
            .unwrap_or_else(|| "unknown".into()),
        "category": manifest
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.categories.first().cloned())
            .unwrap_or_else(|| "utilities".into()),
        "version": manifest.spore.version,
        "badge": presentation.map(|p| p.badge.clone()).unwrap_or_else(|| "?".into()),
        "color": presentation.map(|p| p.color.clone()),
        "state": state,
        "summary": manifest.spore.summary,
        "description": manifest.spore.description.clone(),
        "license": manifest.spore.license,
        "homepage": manifest.spore.homepage.clone(),
        "digest": app.digest,
    })
}

fn display_name(installed: &[Installed], id: &str) -> String {
    installed
        .iter()
        .find(|app| app.id() == id)
        .map(|app| app.manifest.spore.name.clone())
        .unwrap_or_else(|| short_name(id))
}

fn short_name(id: &str) -> String {
    id.rsplit('.').next().unwrap_or(id).to_owned()
}

fn label(id: &str) -> String {
    id.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render a JSON value as a Lua table.
///
/// Keys are quoted, `null` is omitted rather than written as `nil` (which would
/// leave a hole in an array), and ordering is `serde_json`'s, so the same
/// snapshot always produces the same file.
pub fn to_lua(value: &Value) -> String {
    match value {
        Value::Null => "nil".into(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => quote(text),
        Value::Array(items) => {
            let body = items.iter().map(to_lua).collect::<Vec<_>>().join(", ");
            format!("{{{body}}}")
        }
        Value::Object(map) => {
            let body = map
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| format!("[{}] = {}", quote(key), to_lua(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{body}}}")
        }
    }
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\{}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{EntryKind, Index};
    use crate::plan::plan;

    const CID: &str = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";

    fn installed(version: &str) -> Installed {
        let manifest = soil_spore::Manifest::parse(&format!(
            "schema = 1\n[spore]\nid = \"dev.fungos.demo\"\nname = \"demo\"\nversion = \"{version}\"\nlicense = \"MIT\"\nsummary = \"A demo Spore\"\ndescription = \"It does the demo.\"\n[engine]\nsoil = \">=0.1\"\ntarget = \"wasm32-wasip2\"\n[ui]\nentry = \"ui/demo.lua\"\n[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n[grants]\n\"soil:canvas\" = \"present\"\n[presentation]\nbadge = \"D\"\ncolor = \"#8b9dff\"\n[catalog]\ncategories = [\"productivity\"]\nkeywords = [\"demo\"]\n"
        ))
        .expect("manifest");
        Installed {
            manifest,
            root: std::path::PathBuf::new(),
            digest: "1".repeat(64),
            module: None,
            ui: Some("a".into()),
            publisher: Some("fungos".into()),
            pinned: None,
        }
    }

    fn index(version: &str) -> Index {
        Index::parse(&format!(
            r#"{{"schema":1,"source":"fungos","checkpoint":{{"seq":2,"root":"r","signature":"s"}},"entries":[
                {{"seq":1,"kind":"add","spore_id":"dev.fungos.demo","version":"{version}","digest":"{d}","package":"{CID}","module":null,"ui":"b","grants":["soil:canvas"],"publisher":"fungos"}},
                {{"seq":2,"kind":"add","spore_id":"dev.fungos.other","version":"1.0.0","digest":"{d}","package":"{CID}","publisher":"home-lab"}}
            ]}}"#,
            d = "0".repeat(64)
        ))
        .expect("index")
    }

    #[test]
    fn a_snapshot_carries_the_state_each_row_needs() {
        let catalog = index("0.2.0").catalog();
        let plan = plan(&[installed("0.1.0")], &catalog);
        let source = Source {
            name: "fungOS".into(),
            index: CID.into(),
            verified: true,
        };
        let snapshot = build(&[installed("0.1.0")], &plan, &catalog, &source);
        let apps = snapshot.value["apps"].as_array().unwrap();
        assert_eq!(apps.len(), 2, "installed plus one catalog-only row");
        let demo = apps
            .iter()
            .find(|app| app["id"] == "dev.fungos.demo")
            .unwrap();
        assert_eq!(demo["state"], "update");
        assert_eq!(demo["badge"], "D");
        assert_eq!(demo["category"], "productivity");
        let other = apps
            .iter()
            .find(|app| app["id"] == "dev.fungos.other")
            .unwrap();
        assert_eq!(other["state"], "get");
        assert_eq!(other["name"], "other");
        assert_eq!(snapshot.value["pending"]["total"], 1);
    }

    #[test]
    fn the_category_list_is_the_shared_vocabulary_with_counts() {
        let catalog = index("0.1.0").catalog();
        let app = installed("0.1.0");
        let plan = plan(std::slice::from_ref(&app), &catalog);
        let source = Source {
            name: "fungOS".into(),
            index: CID.into(),
            verified: true,
        };
        let snapshot = build(&[app], &plan, &catalog, &source);
        let categories = snapshot.value["categories"].as_array().unwrap();
        assert_eq!(categories[0]["id"], "all");
        assert_eq!(categories[1]["id"], "updates");
        assert_eq!(categories[2]["id"], "installed");
        let productivity = categories
            .iter()
            .find(|category| category["id"] == "productivity")
            .unwrap();
        assert_eq!(productivity["label"], "Productivity");
        assert!(productivity["count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn lua_output_is_a_table_literal() {
        let value = json!({"a": 1, "b": [true, "x\"y"], "c": null});
        assert_eq!(to_lua(&value), r#"{["a"] = 1, ["b"] = {true, "x\"y"}}"#);
    }

    #[test]
    fn an_empty_catalog_still_builds() {
        let mut empty = BTreeMap::new();
        empty.insert(
            "dev.fungos.demo".to_owned(),
            Entry {
                seq: 1,
                kind: EntryKind::Add,
                spore_id: "dev.fungos.demo".into(),
                version: "0.1.0".into(),
                digest: "0".repeat(64),
                package: CID.into(),
                listing: None,
                module: None,
                ui: None,
                grants: Vec::new(),
                system: None,
                publisher: None,
                signature: String::new(),
            },
        );
        let catalog = empty;
        let plan = plan(&[], &catalog);
        let source = Source {
            name: "fungOS".into(),
            index: CID.into(),
            verified: false,
        };
        let snapshot = build(&[], &plan, &catalog, &source);
        assert!(snapshot.to_lua().starts_with("return {"));
        assert_eq!(snapshot.value["apps"].as_array().unwrap().len(), 1);
    }
}
