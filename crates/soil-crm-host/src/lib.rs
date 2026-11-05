//! Read-only CRM projection over a records book, plus deterministic mock data.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fungos_records::{Record, Store};
use serde_json::{Value, json};

pub const COMPANY_SCHEMA: &str = "fungos.crm/company@1";
pub const ACCOUNT_SCHEMA: &str = "fungos.crm/account@1";
pub const ACCOUNT_COMPANY_SCHEMA: &str = "fungos.crm/account-company@1";
pub const CONTACT_SCHEMA: &str = "fungos.crm/contact@1";
pub const ASSIGNMENT_SCHEMA: &str = "fungos.crm/account-contact@1";
pub const PROJECTION_SCHEMA: &str = "fungos.crm/account-contact-projection@1";
pub const PROJECT_SCHEMA: &str = "fungos.crm/project@1";
pub const PROJECT_COMPANY_SCHEMA: &str = "fungos.crm/project-company@1";
pub const PROJECT_ACCOUNT_SCHEMA: &str = "fungos.crm/project-account@1";
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

/// Link a private contact to an account without creating a team projection.
pub fn link_contact(
    root: &Path,
    book: &str,
    account_id: &str,
    contact_id: &str,
    role: &str,
) -> Result<String> {
    let mut store = Store::open(root, &root.join("device_id"))?;
    let account = store
        .get(book, account_id)
        .with_context(|| format!("account {account_id} not found"))?;
    ensure_schema(&account, ACCOUNT_SCHEMA)?;
    let contact = store
        .get(book, contact_id)
        .with_context(|| format!("contact {contact_id} not found"))?;
    ensure_schema(&contact, CONTACT_SCHEMA)?;
    let token = contact.fields["contact_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("contact {contact_id} has no contact_token"))?;
    let assignment_id = assignment_id(account_id, contact_id);
    store.put_record(
        book,
        &assignment_id,
        ASSIGNMENT_SCHEMA,
        1,
        json!({
            "account_id": account_id,
            "contact_id": contact_id,
            "contact_token": token,
            "role": role,
            "status": "active",
        }),
    )?;
    Ok(assignment_id)
}

/// Remove a private account-contact assignment and any projection for it.
pub fn unlink_contact(root: &Path, book: &str, account_id: &str, contact_id: &str) -> Result<()> {
    let mut store = Store::open(root, &root.join("device_id"))?;
    let assignment_id = assignment_id(account_id, contact_id);
    let assignment = store
        .get(book, &assignment_id)
        .with_context(|| format!("assignment {assignment_id} not found"))?;
    ensure_schema(&assignment, ASSIGNMENT_SCHEMA)?;
    let token = assignment.fields["contact_token"]
        .as_str()
        .unwrap_or_default();
    store.delete_record(book, &assignment_id, ASSIGNMENT_SCHEMA, 1)?;
    let projection_id = projection_id(account_id, contact_id);
    if store.get(book, &projection_id).is_ok() {
        store.delete_record(book, &projection_id, PROJECTION_SCHEMA, 1)?;
    }
    let _ = token;
    Ok(())
}

/// Link an account to one of our companies. The relationship keeps its own
/// owner, roles and status, so one account can work with several brands.
pub fn assign_company(
    root: &Path,
    book: &str,
    account_id: &str,
    company_id: &str,
    roles: &[String],
) -> Result<String> {
    let mut store = Store::open(root, &root.join("device_id"))?;
    let account = store
        .get(book, account_id)
        .with_context(|| format!("account {account_id} not found"))?;
    ensure_schema(&account, ACCOUNT_SCHEMA)?;
    let company = store
        .get(book, company_id)
        .with_context(|| format!("company {company_id} not found"))?;
    ensure_schema(&company, COMPANY_SCHEMA)?;
    let id = account_company_id(account_id, company_id);
    let roles = if roles.is_empty() {
        vec!["customer".to_string()]
    } else {
        roles.to_vec()
    };
    store.put_record(
        book,
        &id,
        ACCOUNT_COMPANY_SCHEMA,
        1,
        json!({
            "account_id": account_id,
            "company_id": company_id,
            "roles": roles,
            "status": "active",
            "owner": account.fields["owner"],
            "source": "manual",
            "visibility": "team",
        }),
    )?;
    Ok(id)
}

/// Remove an account-to-company relationship.
pub fn unassign_company(root: &Path, book: &str, account_id: &str, company_id: &str) -> Result<()> {
    let mut store = Store::open(root, &root.join("device_id"))?;
    let id = account_company_id(account_id, company_id);
    let record = store
        .get(book, &id)
        .with_context(|| format!("account-company {id} not found"))?;
    ensure_schema(&record, ACCOUNT_COMPANY_SCHEMA)?;
    store.delete_record(book, &id, ACCOUNT_COMPANY_SCHEMA, 1)?;
    Ok(())
}

fn ensure_schema(record: &Record, schema: &str) -> Result<()> {
    if record.schema_id != schema {
        anyhow::bail!(
            "record {} has schema {}, expected {schema}",
            record.entity_id,
            record.schema_id
        );
    }
    Ok(())
}

fn assignment_id(account_id: &str, contact_id: &str) -> String {
    format!("ac-{account_id}-{contact_id}")
}

fn projection_id(account_id: &str, contact_id: &str) -> String {
    format!("proj-{account_id}-{contact_id}")
}

fn account_company_id(account_id: &str, company_id: &str) -> String {
    format!("acm-{account_id}-{company_id}")
}

fn project_account_id(project_id: &str, account_id: &str) -> String {
    format!("pa-{project_id}-{account_id}")
}

fn project_company_id(project_id: &str, company_id: &str) -> String {
    format!("pc-{project_id}-{company_id}")
}

pub fn snapshot(root: &Path, book: &str, view: &str) -> Result<Value> {
    let store = Store::open(root, &root.join("device_id"))?;
    let records = store
        .list(book, None, 10_000)
        .with_context(|| format!("read {book}"))?;
    let team = view == "team";

    let companies: BTreeMap<String, &Record> = records
        .iter()
        .filter(|record| record.schema_id == COMPANY_SCHEMA)
        .map(|record| (record.entity_id.clone(), record))
        .collect();
    let accounts: BTreeMap<String, &Record> = records
        .iter()
        .filter(|record| record.schema_id == ACCOUNT_SCHEMA)
        .map(|record| (record.entity_id.clone(), record))
        .collect();
    let account_companies = records
        .iter()
        .filter(|record| record.schema_id == ACCOUNT_COMPANY_SCHEMA)
        .collect::<Vec<_>>();

    let projections = records
        .iter()
        .filter(|record| record.schema_id == PROJECTION_SCHEMA)
        .map(projection)
        .collect::<Vec<_>>();
    let projections_by_token: BTreeMap<String, &Value> = projections
        .iter()
        .filter_map(|projection| {
            projection["contact_token"]
                .as_str()
                .map(|token| (token.to_string(), projection))
        })
        .collect();
    let contacts_by_id: BTreeMap<String, &Record> = records
        .iter()
        .filter(|record| record.schema_id == CONTACT_SCHEMA)
        .map(|record| (record.entity_id.clone(), record))
        .collect();
    let contacts = if team {
        projections.clone()
    } else {
        let mut contacts = Vec::new();
        for assignment in records
            .iter()
            .filter(|record| record.schema_id == ASSIGNMENT_SCHEMA)
        {
            let Some(contact_id) = assignment.fields["contact_id"].as_str() else {
                continue;
            };
            let Some(record) = contacts_by_id.get(contact_id) else {
                continue;
            };
            let mut value = contact(record);
            value["account_id"] = assignment.fields["account_id"].clone();
            value["role"] = assignment.fields["role"].clone();
            value["shared"] = Value::Bool(
                assignment.fields["contact_token"]
                    .as_str()
                    .is_some_and(|token| projections_by_token.contains_key(token)),
            );
            contacts.push(value);
        }
        contacts
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

    let projects_by_company = company_projects(&records, team);
    let accounts_by_company = company_accounts(
        &records,
        &accounts,
        &account_companies,
        &contacts,
        &activities,
        &opportunities,
        &projects_by_company,
    );
    let assigned_accounts = account_companies
        .iter()
        .filter_map(|record| record.fields["account_id"].as_str())
        .collect::<BTreeSet<_>>();

    let mut out = Vec::new();
    for (id, record) in companies {
        let mut company = company(record);
        company["id"] = Value::String(id.clone());
        company["accounts"] =
            Value::Array(accounts_by_company.get(&id).cloned().unwrap_or_default());
        company["projects"] =
            Value::Array(projects_by_company.get(&id).cloned().unwrap_or_default());
        company["opportunities"] = Value::Array(
            opportunities
                .iter()
                .filter(|opportunity| opportunity["company_id"] == id)
                .cloned()
                .collect(),
        );
        out.push(company);
    }

    let mut unassigned = accounts
        .iter()
        .filter(|(id, _)| !assigned_accounts.contains(id.as_str()))
        .map(|(id, record)| {
            account_for_company(id, record, None, &contacts, &activities, &opportunities)
        })
        .collect::<Vec<_>>();
    if !unassigned.is_empty() {
        unassigned.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        out.push(json!({
            "id": "co-unassigned",
            "name": "Unassigned",
            "kind": "unassigned",
            "status": "active",
            "accounts": unassigned,
            "projects": [],
            "opportunities": [],
        }));
    }

    out.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({
        "view": if team { "team" } else { "private" },
        "companies": out,
    }))
}

fn company_accounts(
    records: &[Record],
    accounts: &BTreeMap<String, &Record>,
    account_companies: &[&Record],
    contacts: &[Value],
    activities: &[Value],
    opportunities: &[Value],
    projects_by_company: &BTreeMap<String, Vec<Value>>,
) -> BTreeMap<String, Vec<Value>> {
    let mut by_company: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for link in account_companies {
        let Some(account_id) = link.fields["account_id"].as_str() else {
            continue;
        };
        let Some(company_id) = link.fields["company_id"].as_str() else {
            continue;
        };
        let Some(record) = accounts.get(account_id) else {
            continue;
        };
        let projects = projects_by_company
            .get(company_id)
            .map(|projects| {
                projects
                    .iter()
                    .filter(|project| {
                        project["accounts"].as_array().is_some_and(|accounts| {
                            accounts.iter().any(|account| account["id"] == account_id)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut value = account_for_company(
            account_id,
            record,
            Some(link),
            contacts,
            activities,
            opportunities,
        );
        value["projects"] = Value::Array(projects);
        by_company
            .entry(company_id.to_string())
            .or_default()
            .push(value);
    }
    for values in by_company.values_mut() {
        values.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    }
    let _ = records;
    by_company
}

fn account_for_company(
    id: &str,
    record: &Record,
    link: Option<&Record>,
    contacts: &[Value],
    activities: &[Value],
    opportunities: &[Value],
) -> Value {
    let company_id = link.and_then(|link| link.fields["company_id"].as_str());
    let mut account = account(record);
    account["id"] = Value::String(id.to_string());
    account["relationship"] = link
        .map(|link| {
            json!({
                "company_id": link.fields["company_id"],
                "roles": link.fields["roles"],
                "status": link.fields["status"],
                "owner": link.fields["owner"],
                "source": link.fields["source"],
                "first_interaction": link.fields["first_interaction"],
                "last_interaction": link.fields["last_interaction"],
            })
        })
        .unwrap_or(Value::Null);
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
            .filter(|activity| match company_id {
                Some(company_id) => {
                    activity["company_id"] == company_id || activity["company_id"].is_null()
                }
                None => activity["company_id"].is_null(),
            })
            .cloned()
            .collect(),
    );
    account["opportunities"] = Value::Array(
        opportunities
            .iter()
            .filter(|opportunity| opportunity["account_id"] == id)
            .filter(|opportunity| match company_id {
                Some(company_id) => {
                    opportunity["company_id"] == company_id || opportunity["company_id"].is_null()
                }
                None => opportunity["company_id"].is_null(),
            })
            .cloned()
            .collect(),
    );
    account
}

fn company_projects(records: &[Record], team: bool) -> BTreeMap<String, Vec<Value>> {
    let projects: BTreeMap<String, &Record> = records
        .iter()
        .filter(|record| record.schema_id == PROJECT_SCHEMA)
        .filter(|record| !team || record.fields["visibility"] != "private")
        .map(|record| (record.entity_id.clone(), record))
        .collect();
    let project_company_links = records
        .iter()
        .filter(|record| record.schema_id == PROJECT_COMPANY_SCHEMA)
        .collect::<Vec<_>>();

    let mut accounts_by_project: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for link in records
        .iter()
        .filter(|record| record.schema_id == PROJECT_ACCOUNT_SCHEMA)
    {
        let Some(project_id) = link.fields["project_id"].as_str() else {
            continue;
        };
        accounts_by_project
            .entry(project_id.to_string())
            .or_default()
            .push(json!({
                "id": link.fields["account_id"],
                "name": link.fields["account_name"],
                "role": link.fields["role"],
                "primary": link.fields["primary"],
            }));
    }

    let mut project_ids_by_company: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (project_id, record) in &projects {
        if let Some(company_id) = record.fields["company_id"].as_str() {
            project_ids_by_company
                .entry(company_id.to_string())
                .or_default()
                .insert(project_id.clone());
        }
    }
    for link in &project_company_links {
        let (Some(project_id), Some(company_id)) = (
            link.fields["project_id"].as_str(),
            link.fields["company_id"].as_str(),
        ) else {
            continue;
        };
        if projects.contains_key(project_id) {
            project_ids_by_company
                .entry(company_id.to_string())
                .or_default()
                .insert(project_id.to_string());
        }
    }

    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (company_id, project_ids) in project_ids_by_company {
        let mut values = Vec::new();
        for project_id in project_ids {
            let Some(record) = projects.get(&project_id) else {
                continue;
            };
            let mut value = project(record);
            value["id"] = Value::String(project_id.clone());
            value["accounts"] = Value::Array(
                accounts_by_project
                    .get(&project_id)
                    .cloned()
                    .unwrap_or_default(),
            );
            value["company_role"] = if record.fields["company_id"] == company_id {
                Value::String("owner".into())
            } else {
                project_company_links
                    .iter()
                    .find(|link| {
                        link.fields["project_id"] == project_id
                            && link.fields["company_id"] == company_id
                    })
                    .and_then(|link| link.fields["role"].as_str())
                    .map(|role| Value::String(role.to_string()))
                    .unwrap_or_else(|| Value::String("participant".into()))
            };
            values.push(value);
        }
        values.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        out.insert(company_id, values);
    }
    out
}

fn account(record: &Record) -> Value {
    json!({
        "name": record.fields["name"],
        "legal_name": record.fields["legal_name"],
        "domains": record.fields["domains"],
        "website": record.fields["website"],
        "industry": record.fields["industry"],
        "phone": record.fields["phone"],
        "description": record.fields["description"],
        "parent_account_id": record.fields["parent_account_id"],
        "owner": record.fields["owner"],
        "visibility": record.fields["visibility"],
        "hidden": record.fields["hidden"],
        "first_interaction": record.fields["first_interaction"],
        "last_interaction": record.fields["last_interaction"],
    })
}

fn company(record: &Record) -> Value {
    json!({
        "name": record.fields["name"],
        "slug": record.fields["slug"],
        "legal_name": record.fields["legal_name"],
        "kind": record.fields["kind"],
        "parent_company_id": record.fields["parent_company_id"],
        "brands": record.fields["brands"],
        "domains": record.fields["domains"],
        "status": record.fields["status"],
    })
}

fn project(record: &Record) -> Value {
    json!({
        "name": record.fields["name"],
        "company_id": record.fields["company_id"],
        "kind": record.fields["kind"],
        "status": record.fields["status"],
        "owner": record.fields["owner"],
        "summary": record.fields["summary"],
        "start_at": record.fields["start_at"],
        "due_at": record.fields["due_at"],
        "visibility": record.fields["visibility"],
    })
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
        "shared": false,
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
        "shared": true,
    })
}

fn activity(record: &Record) -> Value {
    json!({
        "id": record.entity_id,
        "company_id": record.fields["company_id"],
        "account_id": record.fields["account_id"],
        "project_id": record.fields["project_id"],
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
        "company_id": record.fields["company_id"],
        "account_id": record.fields["account_id"],
        "project_id": record.fields["project_id"],
        "name": record.fields["name"],
        "stage": record.fields["stage"],
        "amount": record.fields["amount"],
        "close_date": record.fields["close_date"],
        "visibility": record.fields["visibility"],
    })
}

fn mock_records() -> Vec<(String, &'static str, Value)> {
    let mut records = Vec::new();
    let companies = [
        ("co-fpl", "FPL", "fpl"),
        ("co-puget-audio", "Puget Audio", "puget-audio"),
        ("co-new-ancients", "New Ancients", "new-ancients"),
        ("co-gsg", "GSG", "gsg"),
        ("co-cft", "CFT", "cft"),
    ];
    for (id, name, slug) in companies {
        records.push((
            id.into(),
            COMPANY_SCHEMA,
            json!({
                "name": name,
                "slug": slug,
                "kind": "brand",
                "status": "active",
                "brands": [name],
                "domains": [],
            }),
        ));
    }

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

    let account_companies = [
        ("a-acme", "co-fpl", ["customer"]),
        ("a-acme", "co-puget-audio", ["customer"]),
        ("a-northstar", "co-puget-audio", ["customer"]),
        ("a-northstar", "co-new-ancients", ["partner"]),
        ("a-harbor", "co-fpl", ["customer"]),
        ("a-harbor", "co-gsg", ["prospect"]),
    ];
    for (account_id, company_id, roles) in account_companies {
        records.push((
            account_company_id(account_id, company_id),
            ACCOUNT_COMPANY_SCHEMA,
            json!({
                "account_id": account_id,
                "company_id": company_id,
                "roles": roles,
                "status": "active",
                "owner": "Avery",
                "source": "mock",
                "first_interaction": "2026-10-01T09:00:00Z",
                "last_interaction": "2026-11-04T16:20:00Z",
                "visibility": "team",
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
            assignment_id(account, id),
            ASSIGNMENT_SCHEMA,
            json!({
                "account_id": account,
                "contact_id": id,
                "contact_token": token,
                "role": title,
                "status": "active",
            }),
        ));
        if id == "c-ada" {
            records.push((
                projection_id(account, id),
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
    }

    let projects = [
        (
            "p-acme-pilot",
            "co-fpl",
            "Acme Robotics pilot",
            "customer",
            "active",
            "Deployment pilot for the robotics line.",
            "2026-10-15",
            "2026-12-15",
        ),
        (
            "p-acme-listening-room",
            "co-puget-audio",
            "Acme listening room",
            "customer",
            "proposed",
            "Reference system for the new demo space.",
            "2026-11-10",
            "2027-02-01",
        ),
        (
            "p-northstar-studio",
            "co-puget-audio",
            "Northstar studio rollout",
            "customer",
            "active",
            "Control room and tracking room refresh.",
            "2026-09-20",
            "2027-01-20",
        ),
        (
            "p-harbor-intake",
            "co-fpl",
            "Harbor intake workflow",
            "customer",
            "active",
            "Front-desk scheduling and patient intake.",
            "2026-10-10",
            "2026-11-30",
        ),
        (
            "p-ancients-refresh",
            "co-new-ancients",
            "Brand system refresh",
            "internal",
            "active",
            "Shared identity and asset system across the group.",
            "2026-08-01",
            "2026-12-01",
        ),
    ];
    for (id, company_id, name, kind, status, summary, start_at, due_at) in projects {
        records.push((
            id.into(),
            PROJECT_SCHEMA,
            json!({
                "company_id": company_id,
                "name": name,
                "kind": kind,
                "status": status,
                "owner": "Avery",
                "summary": summary,
                "start_at": start_at,
                "due_at": due_at,
                "visibility": "team",
                "source": "mock",
            }),
        ));
    }

    let project_accounts = [
        ("p-acme-pilot", "a-acme", "Acme Robotics", "customer", true),
        (
            "p-acme-listening-room",
            "a-acme",
            "Acme Robotics",
            "customer",
            true,
        ),
        (
            "p-northstar-studio",
            "a-northstar",
            "Northstar Studio",
            "customer",
            true,
        ),
        (
            "p-harbor-intake",
            "a-harbor",
            "Harbor Dental",
            "customer",
            true,
        ),
    ];
    for (project_id, account_id, account_name, role, primary) in project_accounts {
        records.push((
            project_account_id(project_id, account_id),
            PROJECT_ACCOUNT_SCHEMA,
            json!({
                "project_id": project_id,
                "account_id": account_id,
                "account_name": account_name,
                "role": role,
                "primary": primary,
            }),
        ));
    }

    records.push((
        project_company_id("p-acme-listening-room", "co-fpl"),
        PROJECT_COMPANY_SCHEMA,
        json!({
            "project_id": "p-acme-listening-room",
            "company_id": "co-fpl",
            "role": "participant",
            "visibility": "team",
        }),
    ));

    let activities = [
        (
            "act-1",
            "co-fpl",
            "p-acme-pilot",
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
            "co-fpl",
            "p-acme-pilot",
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
            "co-fpl",
            "p-acme-pilot",
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
            "co-puget-audio",
            "p-northstar-studio",
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
            "co-fpl",
            "p-harbor-intake",
            "a-harbor",
            "t-elena",
            "call",
            "Harbor front desk follow-up",
            "2026-10-24T11:30:00Z",
            "They want a scheduling integration first.",
            "team",
        ),
        (
            "act-6",
            "co-puget-audio",
            "p-acme-listening-room",
            "a-acme",
            "t-ada",
            "meeting",
            "Listening room scoping",
            "2026-11-01T13:00:00Z",
            "Reviewed room dimensions and playback goals.",
            "team",
        ),
    ];
    for (
        id,
        company_id,
        project_id,
        account,
        token,
        kind,
        name,
        occurred,
        description,
        visibility,
    ) in activities
    {
        records.push((
            id.into(),
            ACTIVITY_SCHEMA,
            json!({
                "company_id": company_id,
                "project_id": project_id,
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
            "co-fpl",
            "p-acme-pilot",
            "a-acme",
            "Robotics pilot",
            "Demo",
            24000,
            "2026-12-15",
        ),
        (
            "op-2",
            "co-puget-audio",
            "p-northstar-studio",
            "a-northstar",
            "Studio rollout",
            "Lead",
            8000,
            "2027-01-20",
        ),
        (
            "op-3",
            "co-fpl",
            "p-harbor-intake",
            "a-harbor",
            "Front desk scheduling",
            "Customer",
            4200,
            "2026-11-30",
        ),
        (
            "op-4",
            "co-puget-audio",
            "p-acme-listening-room",
            "a-acme",
            "Listening room system",
            "Proposal",
            16000,
            "2027-01-10",
        ),
    ];
    for (id, company_id, project_id, account, name, stage, amount, close_date) in opportunities {
        records.push((
            id.into(),
            OPPORTUNITY_SCHEMA,
            json!({
                "company_id": company_id,
                "project_id": project_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linking_is_private_until_explicitly_projected() {
        let root = tempfile::tempdir().unwrap();
        seed_mock(root.path(), "personal/test/crm").unwrap();
        unlink_contact(root.path(), "personal/test/crm", "a-acme", "c-grace").unwrap();
        link_contact(
            root.path(),
            "personal/test/crm",
            "a-harbor",
            "c-grace",
            "Advisor",
        )
        .unwrap();

        let team = snapshot(root.path(), "personal/test/crm", "team").unwrap();
        let private = snapshot(root.path(), "personal/test/crm", "private").unwrap();
        let harbor_team = company_account(&team, "co-fpl", "a-harbor");
        assert!(harbor_team["contacts"].as_array().unwrap().is_empty());
        let harbor_private = company_account(&private, "co-fpl", "a-harbor");
        let grace = harbor_private["contacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|contact| contact["id"] == "c-grace")
            .unwrap();
        assert_eq!(grace["shared"], false);
        assert_eq!(grace["email"], "grace@acme.example");
    }

    #[test]
    fn mock_snapshot_has_private_and_team_views() {
        let root = tempfile::tempdir().unwrap();
        seed_mock(root.path(), "personal/test/crm").unwrap();
        let team = snapshot(root.path(), "personal/test/crm", "team").unwrap();
        let private = snapshot(root.path(), "personal/test/crm", "private").unwrap();
        assert_eq!(team["companies"].as_array().unwrap().len(), 5);
        let team_contact = &company_account(&team, "co-fpl", "a-acme")["contacts"][0];
        assert!(team_contact.get("email").is_none(), "{team_contact}");
        let private_contact = &company_account(&private, "co-fpl", "a-acme")["contacts"][0];
        assert!(private_contact.get("email").is_some(), "{private_contact}");
    }

    #[test]
    fn companies_group_accounts_projects_and_opportunities() {
        let root = tempfile::tempdir().unwrap();
        seed_mock(root.path(), "personal/test/crm").unwrap();
        let team_snapshot = snapshot(root.path(), "personal/test/crm", "team").unwrap();

        let fpl = company(&team_snapshot, "co-fpl");
        assert_eq!(fpl["accounts"].as_array().unwrap().len(), 2);
        assert_eq!(fpl["projects"].as_array().unwrap().len(), 3);
        assert_eq!(fpl["opportunities"].as_array().unwrap().len(), 2);

        let acme_fpl = company_account(&team_snapshot, "co-fpl", "a-acme");
        assert_eq!(acme_fpl["relationship"]["roles"][0], "customer");
        assert_eq!(acme_fpl["projects"].as_array().unwrap().len(), 2);
        assert_eq!(acme_fpl["activities"].as_array().unwrap().len(), 2);
        let private = snapshot(root.path(), "personal/test/crm", "private").unwrap();
        assert_eq!(
            company_account(&private, "co-fpl", "a-acme")["activities"]
                .as_array()
                .unwrap()
                .len(),
            3
        );

        let puget = company(&team_snapshot, "co-puget-audio");
        assert_eq!(puget["accounts"].as_array().unwrap().len(), 2);
        assert_eq!(puget["projects"].as_array().unwrap().len(), 2);
        let acme_puget = company_account(&team_snapshot, "co-puget-audio", "a-acme");
        assert_eq!(acme_puget["activities"].as_array().unwrap().len(), 1);
        assert_eq!(acme_puget["opportunities"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn imported_accounts_without_a_company_stay_visible() {
        let root = tempfile::tempdir().unwrap();
        let book = "personal/test/crm";
        let mut store = Store::open(root.path(), &root.path().join("device_id")).unwrap();
        store
            .put_record(
                book,
                "a-imported",
                ACCOUNT_SCHEMA,
                1,
                json!({"name": "Imported Co"}),
            )
            .unwrap();
        let snapshot = snapshot(root.path(), book, "team").unwrap();
        let unassigned = company(&snapshot, "co-unassigned");
        assert_eq!(unassigned["accounts"][0]["id"], "a-imported");
    }

    fn company<'a>(snapshot: &'a Value, id: &str) -> &'a Value {
        snapshot["companies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|company| company["id"] == id)
            .unwrap()
    }

    fn company_account<'a>(snapshot: &'a Value, company_id: &str, account: &str) -> &'a Value {
        company(snapshot, company_id)["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == account)
            .unwrap()
    }
}
