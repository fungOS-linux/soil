//! Generic `fungos-records` ingest sink.
//!
//! This is intentionally not CRM-specific. An external native workflow (Elmers,
//! an importer, a collector) posts schema-tagged records; Soil owns durability,
//! revisions and sync. The caller names a book, id, schema and JSON body.
//!
//! ```sh
//! soil-records-ingest \
//!   --root ~/.local/share/fungos/records \
//!   --book personal/avery/crm \
//!   --token-env SOIL_RECORDS_TOKEN \
//!   --listen 127.0.0.1:8791
//! ```
//!
//! ```json
//! POST /api/records
//! {
//!   "book": "personal/avery/crm",
//!   "id": "espo:account:a1",
//!   "schema": "fungos.crm/account@1",
//!   "body": { "name": "Acme" }
//! }
//! ```
//!
//! Batch: `{ "records": [ ... ] }` at `POST /api/records/batch`.

use std::{
    io::Read,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use fungos_records::Store;
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_BATCH: usize = 1_000;

#[derive(Parser)]
#[command(about = "Generic fungos-records ingest sink")]
struct Cli {
    /// fungos-records root.
    #[arg(long, default_value = "~/.local/share/fungos/records")]
    root: String,
    /// Allowed book. Repeatable. Empty means any book.
    #[arg(long, action = clap::ArgAction::Append)]
    book: Vec<String>,
    /// Allowed schema prefix. Repeatable. Empty means any schema.
    #[arg(long = "schema-prefix", action = clap::ArgAction::Append)]
    schema_prefix: Vec<String>,
    /// Bearer token. Prefer --token-env.
    #[arg(long)]
    token: Option<String>,
    /// Environment variable holding the bearer token.
    #[arg(long, default_value = "SOIL_RECORDS_TOKEN")]
    token_env: String,
    /// Allow unauthenticated writes. Only for loopback development.
    #[arg(long)]
    allow_anonymous: bool,
    #[arg(long, default_value = "127.0.0.1:8791")]
    listen: String,
}

struct App {
    store: Mutex<Store>,
    books: Vec<String>,
    schema_prefixes: Vec<String>,
    token: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = expand_home(&cli.root);
    std::fs::create_dir_all(&root).ok();
    let store = Store::open(&root, &root.join("device_id"))
        .with_context(|| format!("open {}", root.display()))?;
    let token = cli
        .token
        .or_else(|| std::env::var(&cli.token_env).ok())
        .filter(|token| !token.is_empty());
    if token.is_none() && !cli.allow_anonymous {
        bail!(
            "no ingest token: set {} or pass --token; use --allow-anonymous only for loopback development",
            cli.token_env
        );
    }
    let listen: SocketAddr = cli.listen.parse().context("parse --listen")?;
    if token.is_none() && !listen.ip().is_loopback() {
        bail!("anonymous ingest is only allowed on a loopback listener");
    }
    let app = Arc::new(App {
        store: Mutex::new(store),
        books: cli.book,
        schema_prefixes: cli.schema_prefix,
        token,
    });
    let server = Server::http(listen).map_err(|error| anyhow::anyhow!("{error}"))?;
    println!(
        "soil-records-ingest: http://{listen} root={}",
        root.display()
    );
    for request in server.incoming_requests() {
        if let Err(error) = handle(request, &app) {
            eprintln!("soil-records-ingest: {error:#}");
        }
    }
    Ok(())
}

fn handle(request: Request, app: &App) -> Result<()> {
    let method = request.method().clone();
    let url = request.url().to_string();
    if url == "/health" {
        return respond_json(request, StatusCode(200), &json!({ "ok": true }));
    }
    if !authorized(&request, app) {
        return respond_json(
            request,
            StatusCode(401),
            &json!({ "ok": false, "error": "unauthorized" }),
        );
    }
    if method != Method::Post {
        return respond_json(
            request,
            StatusCode(405),
            &json!({ "ok": false, "error": "method not allowed" }),
        );
    }
    let payload = match read_json(request) {
        Ok(payload) => payload,
        Err(error) => {
            return respond_json(
                *error.0,
                StatusCode(400),
                &json!({ "ok": false, "error": error.1 }),
            );
        }
    };
    let (request, value) = payload;
    let (records, batch) = if let Some(records) = value.get("records") {
        let Some(records) = records.as_array() else {
            return respond_json(
                request,
                StatusCode(400),
                &json!({ "ok": false, "error": "records must be an array" }),
            );
        };
        if records.len() > MAX_BATCH {
            return respond_json(
                request,
                StatusCode(400),
                &json!({ "ok": false, "error": "batch too large" }),
            );
        }
        (records.clone(), true)
    } else {
        (vec![value], false)
    };

    let mut results = Vec::new();
    let mut applied = 0usize;
    let mut failed = 0usize;
    for record in records {
        match apply(app, &record) {
            Ok(id) => {
                applied += 1;
                results.push(json!({ "ok": true, "id": id }));
            }
            Err(error) => {
                failed += 1;
                results.push(json!({ "ok": false, "error": error.to_string() }));
            }
        }
    }
    let status = if failed == 0 {
        StatusCode(200)
    } else {
        StatusCode(400)
    };
    let body = if batch {
        json!({ "ok": failed == 0, "applied": applied, "failed": failed, "results": results })
    } else {
        results
            .pop()
            .unwrap_or_else(|| json!({ "ok": false, "error": "no record" }))
    };
    respond_json(request, status, &body)
}

fn apply(app: &App, record: &Value) -> Result<String> {
    let book = required_string(record, "book")?;
    let id = required_string(record, "id")?;
    let schema = required_string(record, "schema")?;
    let version = record.get("version").and_then(Value::as_i64).unwrap_or(1);
    if !app.books.is_empty() && !app.books.iter().any(|allowed| allowed == book) {
        bail!("book {book} is not allowed");
    }
    if !app.schema_prefixes.is_empty()
        && !app
            .schema_prefixes
            .iter()
            .any(|prefix| schema.starts_with(prefix))
    {
        bail!("schema {schema} is not allowed");
    }
    let delete = record.get("op").and_then(Value::as_str) == Some("delete")
        || record
            .get("delete")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let mut store = app.store.lock().expect("records store poisoned");
    if delete {
        store.delete_record(book, id, schema, version)?;
    } else {
        let body = record
            .get("body")
            .cloned()
            .with_context(|| format!("record {id} has no body"))?;
        if !body.is_object() {
            bail!("record {id} body must be a JSON object");
        }
        store.put_record(book, id, schema, version, body)?;
    }
    Ok(id.to_string())
}

fn authorized(request: &Request, app: &App) -> bool {
    let Some(expected) = &app.token else {
        return true;
    };
    let Some(header) = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
    else {
        return false;
    };
    header
        .value
        .as_str()
        .strip_prefix("Bearer ")
        .is_some_and(|value| value == expected)
}

fn read_json(
    mut request: Request,
) -> std::result::Result<(Request, Value), (Box<Request>, String)> {
    let mut body = Vec::new();
    if let Err(error) = request
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut body)
    {
        return Err((Box::new(request), format!("read body: {error}")));
    }
    if body.len() > MAX_BODY {
        return Err((Box::new(request), "body too large".to_string()));
    }
    if body.is_empty() {
        return Err((Box::new(request), "empty body".to_string()));
    }
    match serde_json::from_slice(&body) {
        Ok(value) => Ok((request, value)),
        Err(error) => Err((Box::new(request), format!("invalid JSON: {error}"))),
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("missing {field}"))
}

fn respond_json(request: Request, status: StatusCode, body: &Value) -> Result<()> {
    let text = serde_json::to_vec(body)?;
    let header = Header::from_bytes("Content-Type", "application/json")
        .map_err(|_| anyhow::anyhow!("invalid header"))?;
    request.respond(
        Response::from_data(text)
            .with_status_code(status)
            .with_header(header),
    )?;
    Ok(())
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest);
    }
    PathBuf::from(value)
}
