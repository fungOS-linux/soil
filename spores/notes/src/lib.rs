//! Read-only Markdown notes Spore.
//!
//! The host owns the notes root, index, graph and content transport. This Spore
//! names workspace-relative paths and returns bounded JSON for the host/UI.

wit_bindgen::generate!({ path: "wit", world: "notes", generate_all });

use serde_json::{json, Value};
use soil::graph::graph::{self, Direction};
use soil::index::index::{self, Mode, Query};
use soil::vfs::vfs::{self, Kind};
use soil::vfs_mut::vfs_mut;

const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;
const PAGE: u32 = 200;

struct Component;

export!(Component);

impl Guest for Component {
    fn run(args: Vec<String>) -> Result<(), String> {
        let action = args.first().map(String::as_str).unwrap_or("tree");
        match action {
            "tree" => {
                let path = args.get(1).cloned().unwrap_or_default();
                print_json("tree", &tree(&path)?);
            }
            "read" => {
                let path = args
                    .get(1)
                    .ok_or_else(|| "usage: read <path>".to_string())?;
                print_json("document", &document(path)?);
            }
            "outline" => {
                let path = args
                    .get(1)
                    .ok_or_else(|| "usage: outline <path>".to_string())?;
                let doc = document(path)?;
                print_json("outline", &outline(&doc));
            }
            "links" => {
                let path = args
                    .get(1)
                    .ok_or_else(|| "usage: links <path>".to_string())?;
                let doc = document(path)?;
                print_json("links", &links(&doc));
            }
            "present" => {
                let path = args
                    .get(1)
                    .ok_or_else(|| "usage: present <path>".to_string())?;
                let doc = document(path)?;
                print_json("presentation", &presentation(&doc));
            }
            "save" => {
                let path = args
                    .get(1)
                    .ok_or_else(|| "usage: save <path> <version|new|auto> <content>".to_string())?;
                let expected = args
                    .get(2)
                    .ok_or_else(|| "usage: save <path> <version|new|auto> <content>".to_string())?;
                let content = args
                    .get(3)
                    .ok_or_else(|| "usage: save <path> <version|new|auto> <content>".to_string())?;
                if content.len() > 4 * 1024 * 1024 {
                    return Err("note content exceeds the write budget".into());
                }
                let expected_version = match expected.as_str() {
                    "new" | "auto" => None,
                    other => Some(
                        other
                            .parse::<u64>()
                            .map_err(|_| "save: expected version must be an integer, new or auto".to_string())?,
                    ),
                };
                let result = vfs_mut::write(
                    path,
                    content.as_bytes(),
                    "text/markdown",
                    expected_version,
                )
                .map_err(|error| format!("{error:?}"))?;
                print_json("save", &json!({
                    "path": result.path,
                    "version": result.version,
                    "content_id": result.content_id,
                    "size": result.size,
                }));
            }
            "search" => {
                let text = args.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
                if text.trim().is_empty() {
                    return Err("usage: search <text>".into());
                }
                print_json("search", &search(&text)?);
            }
            "backlinks" => {
                let node = args
                    .get(1)
                    .ok_or_else(|| "usage: backlinks <node>".to_string())?;
                print_json("backlinks", &backlinks(node)?);
            }
            other => {
                return Err(format!(
                    "unknown action {other:?}; use tree, read, outline, links, search, backlinks, present or save"
                ))
            }
        }
        Ok(())
    }
}

fn tree(parent: &str) -> Result<Value, String> {
    let mut entries = Vec::new();
    let mut offset = 0u64;
    loop {
        let page = vfs::ls(parent, offset, PAGE).map_err(|error| format!("{error:?}"))?;
        for entry in page.entries {
            entries.push(json!({
                "path": entry.path,
                "name": entry.name,
                "kind": match entry.kind {
                    Kind::File => "file",
                    Kind::Directory => "directory",
                },
                "size": entry.size,
                "media_type": entry.media_type,
                "version": entry.version,
                "updated_at_ms": entry.updated_at_ms,
            }));
        }
        match page.next {
            Some(next) => offset = next,
            None => break,
        }
    }
    Ok(json!({ "parent": parent, "entries": entries }))
}

fn document(path: &str) -> Result<Value, String> {
    let entry = vfs::stat(path).map_err(|error| format!("{error:?}"))?;
    if matches!(entry.kind, Kind::Directory) {
        return Err(format!("{path} is a directory"));
    }
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    loop {
        let chunk = vfs::read(path, offset, 64 * 1024).map_err(|error| format!("{error:?}"))?;
        if bytes.len() as u64 + chunk.data.len() as u64 > MAX_READ_BYTES {
            return Err(format!("{path} exceeds the notes read budget"));
        }
        offset += chunk.data.len() as u64;
        let eof = chunk.eof;
        bytes.extend_from_slice(&chunk.data);
        if eof || chunk.data.is_empty() {
            break;
        }
    }
    let text = String::from_utf8_lossy(&bytes).to_string();
    Ok(json!({
        "path": entry.path,
        "title": title(path, &text),
        "media_type": entry.media_type,
        "size": entry.size,
        "version": entry.version,
        "content_id": entry.content_id,
        "graph_node": graph_node(&entry.path),
        "text": text,
    }))
}

fn presentation(document: &Value) -> Value {
    let text = document
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut slides = Vec::new();
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.trim() == "---" {
            if !lines.is_empty() {
                slides.push(slide(&lines));
                lines.clear();
            }
            continue;
        }
        lines.push(line.to_string());
    }
    if !lines.is_empty() || slides.is_empty() {
        slides.push(slide(&lines));
    }
    json!({
        "path": document.get("path"),
        "title": document.get("title"),
        "slides": slides,
    })
}

fn slide(lines: &[String]) -> Value {
    let title = lines
        .iter()
        .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .or_else(|| {
            lines
                .iter()
                .map(|line| line.trim())
                .find(|line| !line.is_empty())
        })
        .unwrap_or("Untitled");
    json!({
        "title": title,
        "lines": lines,
    })
}

fn outline(document: &Value) -> Value {
    let mut headings = Vec::new();
    if let Some(text) = document.get("text").and_then(Value::as_str) {
        for line in text.lines() {
            let trimmed = line.trim_start();
            let marks = trimmed.chars().take_while(|ch| *ch == '#').count();
            if (1..=6).contains(&marks) && trimmed.chars().nth(marks) == Some(' ') {
                headings.push(json!({
                    "level": marks,
                    "text": trimmed[marks + 1..].trim(),
                }));
            }
        }
    }
    Value::Array(headings)
}

fn links(document: &Value) -> Value {
    let mut out: Vec<String> = Vec::new();
    if let Some(text) = document.get("text").and_then(Value::as_str) {
        let mut rest = text;
        while let Some(start) = rest.find("[[") {
            rest = &rest[start + 2..];
            let Some(end) = rest.find("]]") else { break };
            let target = rest[..end].trim();
            if !target.is_empty() && target.len() <= 512 {
                out.push(target.to_string());
            }
            rest = &rest[end + 2..];
        }
    }
    out.sort();
    out.dedup();
    Value::Array(out.into_iter().map(Value::String).collect())
}

fn search(text: &str) -> Result<Value, String> {
    let page = index::search(&Query {
        text: text.into(),
        mode: Some(Mode::FullText),
        parent: None,
        media_type_prefix: Some("text/".into()),
        offset: 0,
        limit: 50,
    })
    .map_err(|error| format!("{error:?}"))?;
    let hits = page
        .hits
        .into_iter()
        .map(|hit| {
            json!({
                "path": hit.path,
                "score": hit.score,
                "snippet": hit.snippet,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "query": text, "generation": page.generation, "hits": hits }))
}

fn backlinks(node: &str) -> Result<Value, String> {
    let node = graph_node(node);
    let page = graph::neighbors(&node, None, Direction::In, 0, 100)
        .map_err(|error| format!("{error:?}"))?;
    let edges = page
        .edges
        .into_iter()
        .map(|edge| {
            json!({
                "source": edge.source,
                "target": edge.target,
                "rel": edge.rel,
                "title": edge.title,
                "book": edge.book,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "node": node, "generation": page.generation, "edges": edges }))
}

/// Host graph nodes for files are `vfs:<workspace-path>`. The reader accepts a
/// note path or an explicit locator, so callers do not have to know that mapping.
fn graph_node(node: &str) -> String {
    let node = node.trim();
    if node.contains(':') && !node.starts_with('/') {
        node.to_string()
    } else {
        format!("vfs:{}", node.trim_start_matches('/'))
    }
}

fn title(path: &str, text: &str) -> String {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            if !rest.trim().is_empty() {
                return rest.trim().to_string();
            }
        }
    }
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
        .to_string()
}

fn print_json(label: &str, value: &Value) {
    println!(
        "{label}: {}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}
