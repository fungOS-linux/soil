//! Contacts Spore.
//!
//! Personal contacts are authoritative in the `contacts` namespace. Sharing is
//! explicit and non-destructive: the CRM receives only a first name and opaque
//! token projection. There is no merge and no bulk share.
//!
//! Actions:
//!   list
//!   show <id|token>
//!   add <id> <first> <last> <email> [phone] [title]
//!   set <id|token> <field> <value>
//!   share <account-id> <contact-id|token> [first-name]
//!   unshare <account-id> <contact-id|token>
//!   shared
//!   help

wit_bindgen::generate!({ path: "wit", world: "contacts", generate_all });

use serde_json::{Value, json};
use soil::records::records;

const CONTACT_SCHEMA: &str = "fungos.contacts/contact";
const ACCOUNT_SCHEMA: &str = "fungos.crm/account@1";
const ASSIGNMENT_SCHEMA: &str = "fungos.crm/account-contact@1";
const PROJECTION_SCHEMA: &str = "fungos.crm/account-contact-projection@1";
const AUDIT_SCHEMA: &str = "fungos.audit/event@1";
const DEFAULT_CONTACTS: &str = "contacts";
const DEFAULT_CRM: &str = "crm";
const MAX_RECORDS: usize = 20_000;

struct Component;

export!(Component);

impl Guest for Component {
    fn run(args: Vec<String>) -> Result<(), String> {
        let (contacts, crm, args) = parse_namespaces(args)?;
        let action = args.first().map(String::as_str).unwrap_or("list");
        match action {
            "list" => print_json("contacts", &list(&contacts)?),
            "view" => {
                let selected = args.get(1).map(String::as_str);
                print_json("view", &view(&contacts, &crm, selected)?);
            }
            "show" => {
                let id = args.get(1).ok_or_else(|| "usage: show <id|token>".to_string())?;
                print_json("contact", &show(&contacts, id)?);
            }
            "add" => {
                let value = add(&contacts, &args)?;
                print_json("contact", &value);
            }
            "set" => {
                let value = set(&contacts, &args)?;
                print_json("contact", &value);
            }
            "link" => {
                let value = link(&contacts, &args)?;
                print_json("link", &value);
            }
            "share" => {
                let value = share(&contacts, &crm, &args)?;
                print_json("share", &value);
            }
            "unshare" => {
                let value = unshare(&crm, &args)?;
                print_json("share", &value);
            }
            "shared" => print_json("shared", &shared(&crm)?),
            "help" => print_json("help", &json!({
                "actions": [
                    "list",
                    "view [selected]",
                    "show <id|token>",
                    "add <id> <first> <last> <email> [phone] [title]",
                    "set <id|token> <field> <value>",
                    "link <account-id> <contact-id|token> [role]",
                    "share <account-id> <contact-id|token> [first-name]",
                    "unshare <account-id> <contact-id|token>",
                    "shared",
                ],
                "usage": "--contacts <namespace> --crm <namespace> <action> ...",
                "contacts": DEFAULT_CONTACTS,
                "crm": DEFAULT_CRM,
            })),
            other => return Err(format!("unknown action {other:?}; use help")),
        }
        Ok(())
    }
}

fn parse_namespaces(args: Vec<String>) -> Result<(String, String, Vec<String>), String> {
    let mut contacts = DEFAULT_CONTACTS.to_string();
    let mut crm = DEFAULT_CRM.to_string();
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--contacts" => {
                contacts = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "--contacts requires a namespace".to_string())?
                    .clone();
                index += 2;
            }
            "--crm" => {
                crm = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "--crm requires a namespace".to_string())?
                    .clone();
                index += 2;
            }
            value if value.starts_with("--contacts=") => {
                contacts = value["--contacts=".len()..].to_string();
                index += 1;
            }
            value if value.starts_with("--crm=") => {
                crm = value["--crm=".len()..].to_string();
                index += 1;
            }
            value => {
                rest.push(value.to_string());
                index += 1;
            }
        }
    }
    Ok((contacts, crm, rest))
}

fn list(namespace: &str) -> Result<Value, String> {
    let mut contacts = load_contacts(namespace)?
        .into_iter()
        .map(|(entry, body)| contact_json(&entry.key.id, &body))
        .collect::<Vec<_>>();
    contacts.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({ "contacts": contacts }))
}

fn view(contacts: &str, crm: &str, selected: Option<&str>) -> Result<Value, String> {
    let rows = load_contacts(contacts)?;
    let mut list = rows
        .iter()
        .map(|(entry, body)| {
            let mut value = contact_json(&entry.key.id, body);
            value["selected"] = Value::Bool(selected == Some(entry.key.id.as_str()));
            value
        })
        .collect::<Vec<_>>();
    list.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    let contact = selected
        .and_then(|selected| rows.iter().find(|(entry, body)| {
            entry.key.id == selected
                || body.get("uid").and_then(Value::as_str) == Some(selected)
        }))
        .map(|(entry, body)| {
            let mut value = contact_json(&entry.key.id, body);
            value["revision"] = json!(entry.revision);
            value
        })
        .or_else(|| {
            rows.first().map(|(entry, body)| {
                let mut value = contact_json(&entry.key.id, body);
                value["revision"] = json!(entry.revision);
                value
            })
        })
        .unwrap_or(Value::Null);
    let shared = shared(crm)?;
    Ok(json!({
        "selected": selected,
        "contacts": list,
        "contact": contact,
        "shared": shared.get("contacts").cloned().unwrap_or_else(|| json!([])),
    }))
}

fn show(namespace: &str, id_or_token: &str) -> Result<Value, String> {
    let (entry, body) = find_contact(namespace, id_or_token)?;
    let mut value = contact_json(&entry.key.id, &body);
    value["revision"] = json!(entry.revision);
    Ok(value)
}

fn add(namespace: &str, args: &[String]) -> Result<Value, String> {
    let id = args.get(1).ok_or_else(|| {
        "usage: add <id> <first> <last> <email> [phone] [title]".to_string()
    })?;
    if records::get(&key(namespace, id)).is_ok() {
        return Err(format!("contact {id} already exists"));
    }
    let first = args.get(2).cloned().unwrap_or_default();
    let last = args.get(3).cloned().unwrap_or_default();
    let email = args.get(4).cloned().unwrap_or_default();
    let phone = args.get(5).cloned().unwrap_or_default();
    let title = args.get(6).cloned().unwrap_or_default();
    let token = format!("ct-{id}");
    let body = json!({
        "display_name": format!("{first} {last}").trim(),
        "given_name": first,
        "family_name": last,
        "organization": "",
        "title": title,
        "emails": if email.is_empty() { json!([]) } else { json!([{"label": "work", "value": email}]) },
        "phones": if phone.is_empty() { json!([]) } else { json!([{"label": "work", "value": phone}]) },
        "addresses": [],
        "urls": [],
        "note": "",
        "birthday": "",
        "uid": token,
        "raw_vcard": "",
    });
    let stored = put(namespace, id, CONTACT_SCHEMA, body.clone())?;
    write_audit(namespace, namespace, "contacts.add", id, CONTACT_SCHEMA, stored.revision, None, Some(body))?;
    Ok(json!({
        "id": id,
        "contact_token": token,
        "revision": stored.revision,
    }))
}

fn set(namespace: &str, args: &[String]) -> Result<Value, String> {
    let id_or_token = args
        .get(1)
        .ok_or_else(|| "usage: set <id|token> <field> <value>".to_string())?;
    let field = args
        .get(2)
        .ok_or_else(|| "set: missing field".to_string())?;
    let value = args.get(3).cloned().unwrap_or_default();
    if !matches!(
        field.as_str(),
        "given_name" | "family_name" | "display_name" | "organization" | "title" | "note" | "birthday" | "email" | "phone" | "uid"
    ) {
        return Err(format!("set: unsupported field {field}"));
    }
    let (entry, mut body) = find_contact(namespace, id_or_token)?;
    let before = body.get(field).cloned().unwrap_or(Value::Null);
    match field.as_str() {
        "email" => body["emails"] = labeled_values(&value),
        "phone" => body["phones"] = labeled_values(&value),
        "given_name" | "family_name" => {
            body[field] = Value::String(value.clone());
            let first = body.get("given_name").and_then(Value::as_str).unwrap_or_default();
            let last = body.get("family_name").and_then(Value::as_str).unwrap_or_default();
            body["display_name"] = Value::String(format!("{first} {last}").trim().to_string());
        }
        _ => body[field] = Value::String(value.clone()),
    }
    let stored = put(namespace, &entry.key.id, CONTACT_SCHEMA, body)?;
    write_audit(
        namespace,
        namespace,
        "contacts.set",
        &entry.key.id,
        CONTACT_SCHEMA,
        stored.revision,
        Some(json!({ field: before })),
        Some(json!({ field: value })),
    )?;
    Ok(json!({
        "id": entry.key.id,
        "field": field,
        "value": value,
        "revision": stored.revision,
    }))
}

fn labeled_values(value: &str) -> Value {
    if value.is_empty() {
        json!([])
    } else {
        json!([{ "label": "work", "value": value }])
    }
}

fn link(contacts: &str, args: &[String]) -> Result<Value, String> {
    let account_id = args
        .get(1)
        .ok_or_else(|| "usage: link <account-id> <contact-id|token> [role]".to_string())?;
    let contact_ref = args
        .get(2)
        .ok_or_else(|| "link: missing contact id".to_string())?;
    let role = args.get(3).cloned().unwrap_or_default();
    let (entry, contact) = find_contact(contacts, contact_ref)?;
    let token = contact
        .get("uid")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| format!("contact {} has no uid", entry.key.id))?;
    let assignment_id = format!("ac-{account_id}-{}", entry.key.id);
    let body = json!({
        "account_id": account_id,
        "contact_id": entry.key.id,
        "contact_token": token,
        "role": role,
        "status": "active",
        "source": "contacts-spore",
    });
    let stored = put(contacts, &assignment_id, ASSIGNMENT_SCHEMA, body.clone())?;
    write_audit(
        contacts,
        contacts,
        "contacts.link",
        &assignment_id,
        ASSIGNMENT_SCHEMA,
        stored.revision,
        None,
        Some(json!({
            "account_id": account_id,
            "contact_id": entry.key.id,
            "contact_token": token,
            "role": role,
        })),
    )?;
    Ok(json!({
        "id": assignment_id,
        "account_id": account_id,
        "contact_id": entry.key.id,
        "contact_token": token,
        "role": role,
        "revision": stored.revision,
    }))
}

fn share(contacts: &str, crm: &str, args: &[String]) -> Result<Value, String> {
    let account_id = args
        .get(1)
        .ok_or_else(|| "usage: share <account-id> <contact-id|token> [first-name]".to_string())?;
    let contact_ref = args
        .get(2)
        .ok_or_else(|| "share: missing contact id".to_string())?;
    let account = records::get(&key(crm, account_id)).map_err(|error| {
        format!("account {account_id}: {error:?}")
    })?;
    if account.schema != ACCOUNT_SCHEMA {
        return Err(format!("{account_id} is {}, not an account", account.schema));
    }
    let (entry, contact) = find_contact(contacts, contact_ref)?;
    let token = contact
        .get("uid")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| format!("contact {} has no uid", entry.key.id))?;
    let first_name = args
        .get(3)
        .cloned()
        .filter(|name| !name.is_empty())
        .or_else(|| contact.get("given_name").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| token.to_string());
    let projection_id = format!("proj-{account_id}-{}", entry.key.id);
    let body = json!({
        "contact_token": token,
        "account_id": account_id,
        "first_name": first_name,
        "hidden": false,
    });
    let stored = put(crm, &projection_id, PROJECTION_SCHEMA, body.clone())?;
    write_audit(
        crm,
        crm,
        "crm.contact.share",
        &projection_id,
        PROJECTION_SCHEMA,
        stored.revision,
        None,
        Some(json!({
            "account_id": account_id,
            "contact_token": token,
            "first_name": first_name,
        })),
    )?;
    Ok(json!({
        "id": projection_id,
        "account_id": account_id,
        "contact_token": token,
        "first_name": first_name,
        "revision": stored.revision,
    }))
}

fn unshare(crm: &str, args: &[String]) -> Result<Value, String> {
    let account_id = args
        .get(1)
        .ok_or_else(|| "usage: unshare <account-id> <contact-id|token>".to_string())?;
    let contact_id = args
        .get(2)
        .ok_or_else(|| "unshare: missing contact id".to_string())?;
    let projection_id = format!("proj-{account_id}-{contact_id}");
    let entry = records::get(&key(crm, &projection_id))
        .map_err(|error| format!("share {projection_id}: {error:?}"))?;
    let before: Value = serde_json::from_slice(&entry.body).unwrap_or(Value::Null);
    records::delete(&key(crm, &projection_id))
        .map_err(|error| format!("unshare {projection_id}: {error:?}"))?;
    write_audit(
        crm,
        crm,
        "crm.contact.unshare",
        &projection_id,
        PROJECTION_SCHEMA,
        entry.revision,
        Some(json!({
            "account_id": before.get("account_id").cloned().unwrap_or(Value::Null),
            "contact_token": before.get("contact_token").cloned().unwrap_or(Value::Null),
            "first_name": before.get("first_name").cloned().unwrap_or(Value::Null),
        })),
        None,
    )?;
    Ok(json!({ "id": projection_id, "shared": false }))
}

fn shared(crm: &str) -> Result<Value, String> {
    let mut contacts = load_schema(crm, PROJECTION_SCHEMA)?
        .into_iter()
        .map(|(_, body)| json!({
            "contact_token": body.get("contact_token").cloned().unwrap_or(Value::Null),
            "first_name": body.get("first_name").cloned().unwrap_or(Value::Null),
            "account_id": body.get("account_id").cloned().unwrap_or(Value::Null),
        }))
        .collect::<Vec<_>>();
    contacts.sort_by(|left, right| left["first_name"].as_str().cmp(&right["first_name"].as_str()));
    Ok(json!({ "contacts": contacts }))
}

fn find_contact(namespace: &str, id_or_token: &str) -> Result<(records::Entry, Value), String> {
    let mut cursor = None;
    loop {
        let page = records::scan(namespace, cursor.as_deref(), 500)
            .map_err(|error| format!("contacts {namespace}: {error:?}"))?;
        for entry in page.entries {
            if entry.deleted || entry.schema != CONTACT_SCHEMA {
                continue;
            }
            let body: Value = serde_json::from_slice(&entry.body)
                .map_err(|error| format!("decode {}: {error}", entry.key.id))?;
            if entry.key.id == id_or_token
                || body.get("uid").and_then(Value::as_str) == Some(id_or_token)
            {
                return Ok((entry, body));
            }
        }
        match page.next {
            Some(next) if Some(next.as_str()) != cursor.as_deref() => cursor = Some(next),
            _ => break,
        }
    }
    Err(format!("contact {id_or_token} not found"))
}

fn load_contacts(namespace: &str) -> Result<Vec<(records::Entry, Value)>, String> {
    load_schema(namespace, CONTACT_SCHEMA)
}

fn load_schema(namespace: &str, schema: &str) -> Result<Vec<(records::Entry, Value)>, String> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let page = records::scan(namespace, cursor.as_deref(), 500)
            .map_err(|error| format!("records {namespace}: {error:?}"))?;
        for entry in page.entries {
            if entry.deleted || entry.schema != schema {
                continue;
            }
            let body: Value = serde_json::from_slice(&entry.body)
                .map_err(|error| format!("decode {}: {error}", entry.key.id))?;
            rows.push((entry, body));
            if rows.len() >= MAX_RECORDS {
                return Ok(rows);
            }
        }
        match page.next {
            Some(next) if Some(next.as_str()) != cursor.as_deref() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(rows)
}

fn contact_json(id: &str, body: &Value) -> Value {
    let first = body.get("given_name").and_then(Value::as_str).unwrap_or_default();
    let last = body.get("family_name").and_then(Value::as_str).unwrap_or_default();
    let name = body
        .get("display_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{first} {last}").trim().to_string());
    let email = labeled_value(body.get("emails"));
    let phone = labeled_value(body.get("phones"));
    json!({
        "id": id,
        "name": name,
        "first_name": first,
        "last_name": last,
        "email": email,
        "phone": phone,
        "title": body.get("title").cloned().unwrap_or(Value::Null),
        "organization": body.get("organization").cloned().unwrap_or(Value::Null),
        "note": body.get("note").cloned().unwrap_or(Value::Null),
        "contact_token": body.get("uid").cloned().unwrap_or(Value::Null),
        "uid": body.get("uid").cloned().unwrap_or(Value::Null),
    })
}

fn labeled_value(values: Option<&Value>) -> Value {
    values
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(|value| value.get("value"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn put(namespace: &str, id: &str, schema: &str, body: Value) -> Result<records::Entry, String> {
    let body = serde_json::to_vec(&body).map_err(|error| format!("encode {id}: {error}"))?;
    records::upsert(&records::Entry {
        key: key(namespace, id),
        schema: schema.to_string(),
        revision: 0,
        updated_at_ms: 0,
        origin: String::new(),
        deleted: false,
        body,
    })
    .map_err(|error| format!("write {id}: {error:?}"))
}

fn key(namespace: &str, id: &str) -> records::Key {
    records::Key {
        collection: namespace.to_string(),
        id: id.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_audit(
    audit_namespace: &str,
    target_namespace: &str,
    action: &str,
    target: &str,
    target_schema: &str,
    at_ms: u64,
    before: Option<Value>,
    after: Option<Value>,
) -> Result<(), String> {
    let id = format!("audit-{action}-{target}-{at_ms}");
    let body = serde_json::to_vec(&json!({
        "at_ms": at_ms,
        "action": action,
        "target": target,
        "target_schema": target_schema,
        "target_namespace": target_namespace,
        "result": "ok",
        "before": before,
        "after": after,
        "source": "contacts-spore",
    }))
    .map_err(|error| format!("encode audit: {error}"))?;
    records::upsert(&records::Entry {
        key: key(audit_namespace, &id),
        schema: AUDIT_SCHEMA.to_string(),
        revision: 0,
        updated_at_ms: 0,
        origin: String::new(),
        deleted: false,
        body,
    })
    .map_err(|error| format!("audit {id}: {error:?}"))?;
    Ok(())
}

fn print_json(label: &str, value: &Value) {
    println!("{label}: {}", serde_json::to_string_pretty(value).unwrap_or_default());
}
