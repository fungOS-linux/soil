//! Notification tray and ringer Spore.
//!
//! Notifications are durable/deferrable; rings are active and answerable.
//! The host owns app identity, focus policy, history retention, delivery and
//! the at-most-one answer claim. This module exposes bounded JSON for the UI
//! and host adapters.

wit_bindgen::generate!({ path: "wit", world: "notify", generate_all });

use serde_json::{json, Value};
use soil::notify::cleanup::{self, Scope};
use soil::notify::focus;
use soil::notify::history;
use soil::notify::inbox::{self, Filter};

struct Component;

export!(Component);

impl Guest for Component {
    fn run(args: Vec<String>) -> Result<(), String> {
        let action = args.first().map(String::as_str).unwrap_or("inbox");
        match action {
            "inbox" => {
                let page = inbox::query(&Filter {
                    apps: vec![],
                    categories: vec![],
                    include_held: true,
                    include_read: false,
                    offset: 0,
                    limit: 100,
                })
                .map_err(|error| format!("{error:?}"))?;
                let items = page
                    .items
                    .into_iter()
                    .map(|item| {
                        json!({
                            "id": item.id,
                            "app_id": item.app_id,
                            "title": item.title,
                            "body": item.body,
                            "category": item.category,
                            "thread": item.thread,
                            "group_key": item.group_key,
                            "priority": format!("{:?}", item.priority),
                            "posted_at_ms": item.posted_at_ms,
                            "read": item.read,
                            "held": item.held,
                            "sensitive": item.sensitive,
                            "summary": item.summary,
                            "actions": item.actions.into_iter().map(|action| json!({
                                "id": action.id,
                                "label": action.label,
                                "destructive": action.destructive,
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect::<Vec<_>>();
                print_json("inbox", &json!({ "items": items, "next": page.next }));
            }
            "history" => {
                let page = history::query(0, 100).map_err(|error| format!("{error:?}"))?;
                let entries = page
                    .entries
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "id": entry.id,
                            "app_id": entry.app_id,
                            "title": entry.title,
                            "body": entry.body,
                            "priority": format!("{:?}", entry.priority),
                            "posted_at_ms": entry.posted_at_ms,
                            "dismissed_at_ms": entry.dismissed_at_ms,
                        })
                    })
                    .collect::<Vec<_>>();
                print_json("history", &json!({ "entries": entries, "next": page.next }));
            }
            "focus" => match focus::get().map_err(|error| format!("{error:?}"))? {
                Some(profile) => print_json("focus", &json!({
                    "mode": profile.mode,
                    "until_ms": profile.until_ms,
                    "allow_apps": profile.allow_apps,
                    "allow_categories": profile.allow_categories,
                    "allow_threads": profile.allow_threads,
                    "breakthrough": profile.breakthrough,
                    "rollup_held": profile.rollup_held,
                })),
                None => print_json("focus", &Value::Null),
            },
            "clear" => {
                let scope = match args.get(1).map(String::as_str).unwrap_or("read") {
                    "read" => Scope::Read,
                    "shown" => Scope::Shown,
                    "held" => Scope::Held,
                    "all" => Scope::All,
                    "older-than" => Scope::OlderThan,
                    "app" => Scope::App,
                    "category" => Scope::Category,
                    "archive" => Scope::Archive,
                    other => return Err(format!("unknown cleanup scope {other:?}")),
                };
                let value = args.get(2).cloned();
                let older = args.get(3).and_then(|value| value.parse::<u64>().ok());
                let cleared = cleanup::clear(scope, value.as_deref(), older)
                    .map_err(|error| format!("{error:?}"))?;
                print_json("cleanup", &json!({ "cleared": cleared }));
            }
            other => {
                return Err(format!(
                    "unknown action {other:?}; use inbox, history, focus or clear"
                ))
            }
        }
        Ok(())
    }
}

fn print_json(label: &str, value: &Value) {
    println!(
        "{label}: {}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}
