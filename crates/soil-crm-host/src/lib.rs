//! Read-only CRM projection over a records book, plus deterministic mock data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fungos_records::{Record, Store};
use serde_json::{Value, json};

pub const ACCOUNT_SCHEMA: &str = "fungos.crm/account@1";
pub const CONTACT_SCHEMA: &str = "fungos.crm/contact@1";
pub const ASSIGNMENT_SCHEMA: &str = "fungos.crm/account-contact@1";
pub const PROJECTION_SCHEMA: &str = "fungos.crm/account-contact-projection@1";
pub const ACTIVITY_SCHEMA: &str = "fungos.crm/activity@1";
pub const OPPORTUNITY_SCHEMA: &str = "fungos.crm/opportunity@1";

pub fn seed_mock(root: &Path, book: &str) -> Result<usize> {
    let mut store = Store::open(root, &root.join("device_id"))?;
    let mut count = 0;
    for (id, schema, body) in mock_records() {
        store.put_record(book, &id, schema, 1, body)?;
        count += 1;
    }
    Ok(count)
}

pub fn snapshot(root: &Path, book: &str, view: &str) -> Result<Value> {
    let store = Store::open(root, &root.join("device_id"))?;
    let records = store
        .list(book, None, 10_000)
        .with_context(|| format!("read {book}"))?;
    let team = view == "team";
    let accounts: BTreeMap<String, Value> = records
        .iter()
        .filter(|record| record.schema_id == ACCOUNT_SCHEMA)
        .map(|record| (record.entity_id.clone(), record.fields.clone()))
        .collect();

    let contacts = if team {
        records
            .iter()
            .filter(|record| record.schema_id == PROJECTION_SCHEMA)
            .map(projection)
            .collect::<Vec<_>>()
    } else {
        records
            .iter()
            .filter(|record| record.schema_id == CONTACT_SCHEMA)
            .map(contact)
            .collect::<Vec<_>>()
    };

    let activities = records
        .iter()
        .filter(|record| record.schema_id == ACTIVITY_SCHEMA)
        .filter(|record| !team || record.fields["visibility"] == "team")
        .map(activity)
        .collect::<Vec<_>>();

    let opportunities = records
        .iter()
        .filter(|record| record.schema_id == OPPORTUNITY_SCHEMA)
        .filter(|record| !team || record.fields["visibility"] == "team")
        .map(opportunity)
        .collect::<Vec<_>>();

    let mut out = Vec::new();
    for (id, mut account) in accounts {
        account["contacts"] = Value::Array(
            contacts
                .iter()
                .filter(|contact| contact["account_id"] == id)
                .cloned()
                .collect(),
        );
        account["activities"] = Value::Array(
            activities
                .iter()
                .filter(|activity| activity["account_id"] == id)
                .cloned()
                .collect(),
        );
        account["opportunities"] = Value::Array(
            opportunities
                .iter()
                .filter(|opportunity| opportunity["account_id"] == id)
                .cloned()
                .collect(),
        );
        out.push(account);
    }
    out.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({
        "view": if team { "team" } else { "private" },
        "accounts": out,
    }))
}

fn contact(record: &Record) -> Value {
    json!({
        "id": record.entity_id,
        "contact_token": record.fields["contact_token"],
        "account_id": record.fields["account_id"],
        "first_name": record.fields["first_name"],
        "last_name": record.fields["last_name"],
        "name": record.fields["name"],
        "email": record.fields["email"],
        "phone": record.fields["phone"],
        "title": record.fields["title"],
        "hidden": record.fields["hidden"],
    })
}

fn projection(record: &Record) -> Value {
    json!({
        "id": record.entity_id,
        "contact_token": record.fields["contact_token"],
        "account_id": record.fields["account_id"],
        "first_name": record.fields["first_name"],
        "name": record.fields["first_name"],
        "hidden": record.fields["hidden"],
    })
}

fn activity(record: &Record) -> Value {
    json!({
        "id": record.entity_id,
        "account_id": record.fields["account_id"],
        "contact_token": record.fields["contact_token"],
        "kind": record.fields["kind"],
        "name": record.fields["name"],
        "occurred_at": record.fields["occurred_at"],
        "description": record.fields["description"],
        "visibility": record.fields["visibility"],
    })
}

fn opportunity(record: &Record) -> Value {
    json!({
        "id": record.entity_id,
        "account_id": record.fields["account_id"],
        "name": record.fields["name"],
        "stage": record.fields["stage"],
        "amount": record.fields["amount"],
        "close_date": record.fields["close_date"],
        "visibility": record.fields["visibility"],
    })
}

fn mock_records() -> Vec<(String, &'static str, Value)> {
    let mut records = Vec::new();
    let accounts = [
        ("a-acme", "Acme Robotics", "acme.example", "Manufacturing"),
        (
            "a-northstar",
            "Northstar Studio",
            "northstar.example",
            "Design",
        ),
        ("a-harbor", "Harbor Dental", "harbor.example", "Healthcare"),
    ];
    for (id, name, domain, industry) in accounts {
        records.push((
            id.into(),
            ACCOUNT_SCHEMA,
            json!({
                "name": name,
                "domains": [domain],
                "website": format!("https://{domain}"),
                "industry": industry,
                "owner": "Avery",
                "visibility": "team",
                "hidden": false,
                "first_interaction": "2026-10-01T09:00:00Z",
                "last_interaction": "2026-11-04T16:20:00Z",
            }),
        ));
    }

    let contacts = [
        (
            "c-ada",
            "a-acme",
            "t-ada",
            "Ada",
            "Lovelace",
            "ada@acme.example",
            "+1 555 0101",
            "CTO",
            false,
        ),
        (
            "c-grace",
            "a-acme",
            "t-grace",
            "Grace",
            "Hopper",
            "grace@acme.example",
            "+1 555 0102",
            "VP Engineering",
            false,
        ),
        (
            "c-alan",
            "a-northstar",
            "t-alan",
            "Alan",
            "Turing",
            "alan@northstar.example",
            "+1 555 0103",
            "Founder",
            false,
        ),
        (
            "c-elena",
            "a-harbor",
            "t-elena",
            "Elena",
            "Park",
            "elena@harbor.example",
            "+1 555 0104",
            "Owner",
            false,
        ),
    ];
    for (id, account, token, first, last, email, phone, title, hidden) in contacts {
        records.push((
            id.into(),
            CONTACT_SCHEMA,
            json!({
                "account_id": account,
                "contact_token": token,
                "first_name": first,
                "last_name": last,
                "name": format!("{first} {last}"),
                "email": email,
                "phone": phone,
                "title": title,
                "hidden": hidden,
            }),
        ));
        records.push((
            format!("ac-{id}"),
            ASSIGNMENT_SCHEMA,
            json!({
                "account_id": account,
                "contact_id": id,
                "contact_token": token,
                "role": title,
                "status": "active",
            }),
        ));
        records.push((
            format!("p-{id}"),
            PROJECTION_SCHEMA,
            json!({
                "account_id": account,
                "contact_token": token,
                "first_name": first,
                "status": "active",
                "hidden": hidden,
            }),
        ));
    }

    let activities = [
        (
            "act-1",
            "a-acme",
            "t-ada",
            "call",
            "Discovery call with Ada",
            "2026-11-04T16:20:00Z",
            "Discussed the pilot and procurement. Follow-up next week.",
            "team",
        ),
        (
            "act-2",
            "a-acme",
            "t-grace",
            "email",
            "Sent architecture brief",
            "2026-11-03T10:10:00Z",
            "Shared the deployment brief and asked about SSO.",
            "team",
        ),
        (
            "act-3",
            "a-acme",
            "t-ada",
            "note",
            "Private account note",
            "2026-11-02T09:00:00Z",
            "Do not share the pricing concern outside the deal team.",
            "private",
        ),
        (
            "act-4",
            "a-northstar",
            "t-alan",
            "meeting",
            "Studio intro",
            "2026-10-28T14:00:00Z",
            "Walked through the design workflow.",
            "team",
        ),
        (
            "act-5",
            "a-harbor",
            "t-elena",
            "call",
            "Harbor front desk follow-up",
            "2026-10-24T11:30:00Z",
            "They want a scheduling integration first.",
            "team",
        ),
    ];
    for (id, account, token, kind, name, occurred, description, visibility) in activities {
        records.push((
            id.into(),
            ACTIVITY_SCHEMA,
            json!({
                "account_id": account,
                "contact_token": token,
                "kind": kind,
                "name": name,
                "occurred_at": occurred,
                "description": description,
                "visibility": visibility,
                "source": "mock",
            }),
        ));
    }

    let opportunities = [
        (
            "op-1",
            "a-acme",
            "Robotics pilot",
            "Demo",
            24000,
            "2026-12-15",
        ),
        (
            "op-2",
            "a-northstar",
            "Studio rollout",
            "Lead",
            8000,
            "2027-01-20",
        ),
        (
            "op-3",
            "a-harbor",
            "Front desk scheduling",
            "Customer",
            4200,
            "2026-11-30",
        ),
    ];
    for (id, account, name, stage, amount, close_date) in opportunities {
        records.push((
            id.into(),
            OPPORTUNITY_SCHEMA,
            json!({
                "account_id": account,
                "name": name,
                "stage": stage,
                "amount": amount,
                "close_date": close_date,
                "visibility": "team",
                "source": "mock",
            }),
        ));
    }
    records
}

pub fn default_root() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/fungos/records")
}
