//! EspoCRM -> fungos-records importer.
//!
//! One-way, idempotent import of Espo accounts, contacts, leads,
//! opportunities, calls, meetings, and tasks into schema-tagged records.
//! Credentials stay in the host environment; records never see them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use clap::{Parser, Subcommand};
use fungos_records::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

const ACCOUNT_SCHEMA: &str = "fungos.crm/account@1";
const CONTACT_SCHEMA: &str = "fungos.crm/contact@1";
const LEAD_SCHEMA: &str = "fungos.crm/lead@1";
const OPPORTUNITY_SCHEMA: &str = "fungos.crm/opportunity@1";
const ACTIVITY_SCHEMA: &str = "fungos.crm/activity@1";
const MAX_PAGES: usize = 1000;
const DEFAULT_ENTITIES: &str = "Account,Contact,Lead,Opportunity,Call,Meeting,Task";

#[derive(Parser)]
#[command(version, about = "Import EspoCRM records into fungos-records")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Pull changed Espo records into a records book.
    Sync(SyncArgs),
}

#[derive(Parser)]
struct SyncArgs {
    /// fungos-records root.
    #[arg(long, default_value = "~/.local/share/fungos/records")]
    root: String,
    /// Records book receiving CRM records.
    #[arg(long, default_value = "personal/avery/crm")]
    book: String,
    /// EspoCRM base URL, without `/api/v1`.
    #[arg(long, default_value = "https://crm.fpl.dev")]
    url: String,
    /// Environment variable holding an Espo API key.
    #[arg(long, default_value = "ESPO_API_KEY")]
    api_key_env: String,
    /// Optional basic-auth username env. Used only when no API key is set.
    #[arg(long, default_value = "ESPO_USERNAME")]
    username_env: String,
    /// Optional basic-auth password env.
    #[arg(long, default_value = "ESPO_PASSWORD")]
    password_env: String,
    /// Comma-separated Espo entity names.
    #[arg(long, default_value = DEFAULT_ENTITIES)]
    entities: String,
    /// Page size, bounded by Espo's own limit.
    #[arg(long, default_value_t = 200)]
    page_size: usize,
    /// Force a full import and ignore the local high-water mark.
    #[arg(long)]
    full: bool,
    /// Override the high-water mark for this run.
    #[arg(long)]
    since: Option<String>,
    /// State file. Defaults to `<root>/.soil/espo-sync.json`.
    #[arg(long)]
    state: Option<String>,
    /// Print what would be written without mutating records.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncState {
    #[serde(flatten)]
    entities: BTreeMap<String, String>,
}

#[derive(Clone)]
enum Auth {
    ApiKey(String),
    Basic(String),
}

struct Espo {
    base: Url,
    auth: Auth,
    agent: ureq::Agent,
    page_size: usize,
}

impl Espo {
    fn new(base: &str, auth: Auth, page_size: usize) -> Result<Self> {
        let base = Url::parse(base.trim_end_matches('/')).context("parse Espo URL")?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        Ok(Self {
            base,
            auth,
            agent,
            page_size: page_size.clamp(1, 1000),
        })
    }

    fn request(&self, entity: &str, offset: usize, since: Option<&str>) -> Result<Value> {
        let mut url = self
            .base
            .join(&format!("/api/v1/{entity}"))
            .with_context(|| format!("build URL for {entity}"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("maxSize", &self.page_size.to_string());
            query.append_pair("offset", &offset.to_string());
            query.append_pair("orderBy", "modifiedAt");
            query.append_pair("order", "asc");
            if let Some(since) = since {
                query.append_pair("where[0][type]", "greaterThan");
                query.append_pair("where[0][attribute]", "modifiedAt");
                query.append_pair("where[0][value]", since);
            }
        }
        let mut request = self.agent.get(url.as_str());
        request = match &self.auth {
            Auth::ApiKey(key) => request.set("X-Api-Key", key),
            Auth::Basic(value) => request.set("Authorization", value),
        };
        let response = request.call().map_err(|error| match error {
            ureq::Error::Status(401 | 403, _) => {
                anyhow::anyhow!("Espo authentication failed; check the API key or credentials")
            }
            other => anyhow::anyhow!("Espo request failed: {other}"),
        })?;
        response.into_json().context("decode Espo response")
    }

    fn fetch(&self, entity: &str, since: Option<&str>, dry_run: bool) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let page = self.request(entity, offset, since)?;
            let list = page
                .get("list")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let count = list.len();
            rows.extend(list);
            if count < self.page_size || (dry_run && offset + count >= 1000) {
                break;
            }
            offset += count;
        }
        Ok(rows)
    }
}

fn main() -> Result<()> {
    let Cli { command } = Cli::parse();
    match command {
        Cmd::Sync(args) => sync(args),
    }
}

fn sync(args: SyncArgs) -> Result<()> {
    let root = expand_home(&args.root);
    let book = args.book;
    let auth = auth_from_env(&args.api_key_env, &args.username_env, &args.password_env)?;
    let espo = Espo::new(&args.url, auth, args.page_size)?;
    let state_path = args
        .state
        .as_deref()
        .map(expand_home)
        .unwrap_or_else(|| root.join(".soil/espo-sync.json"));
    let mut state = load_state(&state_path)?;
    let mut store = if args.dry_run {
        None
    } else {
        fs::create_dir_all(&root).ok();
        Some(Store::open(&root, &root.join("device_id"))?)
    };

    let entities: Vec<&str> = args
        .entities
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    if entities.is_empty() {
        bail!("no Espo entities requested");
    }

    let mut total = 0usize;
    for entity in entities {
        let since = if args.full {
            None
        } else {
            args.since
                .clone()
                .or_else(|| state.entities.get(entity).cloned())
        };
        let rows = espo.fetch(entity, since.as_deref(), args.dry_run)?;
        println!(
            "{entity}: {} row(s) since {}",
            rows.len(),
            since.as_deref().unwrap_or("full import")
        );
        let mut high_water = since;
        for row in rows {
            let modified = row
                .get("modifiedAt")
                .and_then(Value::as_str)
                .map(str::to_string);
            let (schema, body) = map_entity(entity, &row)?;
            let id = record_id(entity, &row)?;
            if let Some(store) = store.as_mut() {
                if row.get("deleted").and_then(Value::as_bool).unwrap_or(false) {
                    store.delete_record(&book, &id, schema, 1)?;
                } else if !record_is_current(store, &book, &id, schema, &body)? {
                    store.put_record(&book, &id, schema, 1, body)?;
                }
            }
            if let Some(modified) = modified
                && high_water
                    .as_deref()
                    .is_none_or(|current| modified.as_str() > current)
            {
                high_water = Some(modified);
            }
            total += 1;
        }
        if let Some(high_water) = high_water {
            state.entities.insert(entity.to_string(), high_water);
        }
    }

    if !args.dry_run {
        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&state_path, serde_json::to_vec_pretty(&state)?)
            .with_context(|| format!("write {}", state_path.display()))?;
    }
    println!("imported {total} record(s) into {book}");
    Ok(())
}

fn record_is_current(
    store: &Store,
    book: &str,
    id: &str,
    schema: &str,
    body: &Value,
) -> Result<bool> {
    let Ok(record) = store.get(book, id) else {
        return Ok(false);
    };
    Ok(record.schema_id == schema && record.fields == *body && !record.deleted)
}

fn auth_from_env(api_key_env: &str, username_env: &str, password_env: &str) -> Result<Auth> {
    if let Ok(key) = std::env::var(api_key_env)
        && !key.trim().is_empty()
    {
        return Ok(Auth::ApiKey(key));
    }
    let user = std::env::var(username_env).unwrap_or_default();
    let password = std::env::var(password_env).unwrap_or_default();
    if !user.is_empty() && !password.is_empty() {
        let value = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        return Ok(Auth::Basic(format!("Basic {value}")));
    }
    bail!("no Espo credential: set {api_key_env}, or {username_env} and {password_env}")
}

fn load_state(path: &Path) -> Result<SyncState> {
    if !path.is_file() {
        return Ok(SyncState::default());
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn record_id(entity: &str, row: &Value) -> Result<String> {
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .context("Espo row has no id")?;
    Ok(format!("espo:{}:{id}", entity.to_ascii_lowercase()))
}

fn map_entity(entity: &str, row: &Value) -> Result<(&'static str, Value)> {
    let mapped = match entity {
        "Account" => (ACCOUNT_SCHEMA, account(row)),
        "Contact" => (CONTACT_SCHEMA, contact(row)),
        "Lead" => (LEAD_SCHEMA, lead(row)),
        "Opportunity" => (OPPORTUNITY_SCHEMA, opportunity(row)),
        "Call" | "Meeting" | "Task" => (ACTIVITY_SCHEMA, activity(entity, row)),
        other => bail!("unsupported Espo entity: {other}"),
    };
    Ok(mapped)
}

fn account(row: &Value) -> Value {
    let mut domains = Vec::new();
    if let Some(host) = website_host(string(row, "website")) {
        domains.push(host);
    }
    json!({
        "name": string(row, "name"),
        "domains": domains,
        "website": string(row, "website"),
        "type": string(row, "type"),
        "industry": string(row, "industry"),
        "phone": string(row, "phoneNumber"),
        "description": string(row, "description"),
        "owner": string(row, "assignedUserName"),
        "source": "espo",
        "source_id": string(row, "id"),
        "source_url": concat_opt(string(row, "website")),
        "created_at": string(row, "createdAt"),
        "modified_at": string(row, "modifiedAt"),
    })
}

fn contact(row: &Value) -> Value {
    json!({
        "first_name": string(row, "firstName"),
        "last_name": string(row, "lastName"),
        "name": display_name(row),
        "email": string(row, "emailAddress"),
        "phone": string(row, "phoneNumber"),
        "title": string(row, "title"),
        "account_id": string(row, "accountId"),
        "account_name": string(row, "accountName"),
        "description": string(row, "description"),
        "address": address(row),
        "owner": string(row, "assignedUserName"),
        "source": "espo",
        "source_id": string(row, "id"),
        "created_at": string(row, "createdAt"),
        "modified_at": string(row, "modifiedAt"),
    })
}

fn lead(row: &Value) -> Value {
    json!({
        "first_name": string(row, "firstName"),
        "last_name": string(row, "lastName"),
        "name": display_name(row),
        "email": string(row, "emailAddress"),
        "phone": string(row, "phoneNumber"),
        "title": string(row, "title"),
        "company": string(row, "accountName"),
        "status": string(row, "status"),
        "source": "espo",
        "source_id": string(row, "id"),
        "created_at": string(row, "createdAt"),
        "modified_at": string(row, "modifiedAt"),
    })
}

fn opportunity(row: &Value) -> Value {
    json!({
        "name": string(row, "name"),
        "amount": number(row, "amount"),
        "stage": string(row, "stage"),
        "probability": number(row, "probability"),
        "close_date": string(row, "closeDate"),
        "account_id": string(row, "accountId"),
        "account_name": string(row, "accountName"),
        "owner": string(row, "assignedUserName"),
        "source": "espo",
        "source_id": string(row, "id"),
        "created_at": string(row, "createdAt"),
        "modified_at": string(row, "modifiedAt"),
    })
}

fn activity(entity: &str, row: &Value) -> Value {
    json!({
        "kind": entity.to_ascii_lowercase(),
        "name": string(row, "name"),
        "status": string(row, "status"),
        "date_start": string(row, "dateStart"),
        "date_end": string(row, "dateEnd"),
        "duration": number(row, "duration"),
        "description": string(row, "description"),
        "parent_type": string(row, "parentType"),
        "parent_id": string(row, "parentId"),
        "owner": string(row, "assignedUserName"),
        "source": "espo",
        "source_id": string(row, "id"),
        "created_at": string(row, "createdAt"),
        "modified_at": string(row, "modifiedAt"),
    })
}

fn string(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number(row: &Value, key: &str) -> Value {
    row.get(key).cloned().unwrap_or(Value::Null)
}

fn display_name(row: &Value) -> String {
    let name = string(row, "name");
    if !name.is_empty() {
        return name;
    }
    let first = string(row, "firstName");
    let last = string(row, "lastName");
    format!("{first} {last}").trim().to_string()
}

fn address(row: &Value) -> String {
    [
        string(row, "addressStreet"),
        string(row, "addressCity"),
        string(row, "addressState"),
        string(row, "addressPostalCode"),
        string(row, "addressCountry"),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(", ")
}

fn website_host(website: String) -> Option<String> {
    if website.is_empty() {
        return None;
    }
    let value = if website.contains("://") {
        website
    } else {
        format!("https://{website}")
    };
    Url::parse(&value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
}

fn concat_opt(value: String) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        Value::String(value)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn serve(
        responses: Vec<&'static str>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            for body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0u8; 8192];
                let n = stream.read(&mut buffer).unwrap();
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buffer[..n]).to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), requests, handle)
    }

    #[test]
    fn maps_espo_entities_to_records() {
        let row = json!({
            "id": "c1",
            "firstName": "Ada",
            "lastName": "Lovelace",
            "emailAddress": "ada@example.invalid",
            "phoneNumber": "555-0100",
            "title": "CTO",
            "accountId": "a1",
            "accountName": "Acme",
            "createdAt": "2026-01-01T00:00:00Z",
            "modifiedAt": "2026-01-02T00:00:00Z"
        });
        let (schema, body) = map_entity("Contact", &row).unwrap();
        assert_eq!(schema, CONTACT_SCHEMA);
        assert_eq!(body["name"], "Ada Lovelace");
        assert_eq!(body["email"], "ada@example.invalid");
        assert_eq!(body["account_id"], "a1");
        assert_eq!(record_id("Contact", &row).unwrap(), "espo:contact:c1");
    }

    #[test]
    fn imports_accounts_and_contacts_idempotently() {
        let account_page = r#"{"list":[{"id":"a1","name":"Acme","website":"https://acme.example","modifiedAt":"2026-01-02T00:00:00Z"}]}"#;
        let contact_page = r#"{"list":[{"id":"c1","firstName":"Ada","lastName":"Lovelace","emailAddress":"ada@acme.example","accountId":"a1","modifiedAt":"2026-01-03T00:00:00Z"}]}"#;
        let (url, requests, handle) = serve(vec![account_page, contact_page]);
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state.json");
        let args = SyncArgs {
            root: root.path().display().to_string(),
            book: "personal/test/crm".into(),
            url,
            api_key_env: "UNUSED_ESPO_KEY".into(),
            username_env: "UNUSED_ESPO_USER".into(),
            password_env: "UNUSED_ESPO_PASS".into(),
            entities: "Account,Contact".into(),
            page_size: 200,
            full: true,
            since: None,
            state: Some(state.display().to_string()),
            dry_run: false,
        };
        unsafe { std::env::set_var("UNUSED_ESPO_KEY", "test-key") };
        sync(args).unwrap();
        handle.join().unwrap();
        let store = Store::open(root.path(), &root.path().join("device_id")).unwrap();
        let account = store.get("personal/test/crm", "espo:account:a1").unwrap();
        let contact = store.get("personal/test/crm", "espo:contact:c1").unwrap();
        assert_eq!(account.schema_id, ACCOUNT_SCHEMA);
        assert_eq!(contact.fields["name"], "Ada Lovelace");
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.contains("X-Api-Key: test-key"))
        );
    }

    #[test]
    fn parses_since_state() {
        let state = SyncState {
            entities: BTreeMap::from([("Contact".into(), "2026-01-01T00:00:00Z".into())]),
        };
        let text = serde_json::to_string(&state).unwrap();
        assert!(text.contains("Contact"));
    }
}
