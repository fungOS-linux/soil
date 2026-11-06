//! CRM Spore.
//!
//! Reads only namespaced records: `crm`, `contacts`, `calls`, `messages`.
//! The host maps those names to books and grants read/write per namespace.
//! Contacts, calls and mail stay in personal books; the shared CRM holds the
//! account/project relationships and references them by stable ids/tokens.
//!
//! Actions:
//!   accounts
//!   account <account-id>
//!   timeline <account-id> [limit]
//!   pipeline
//!   help

wit_bindgen::generate!({ path: "wit", world: "crm", generate_all });

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use soil::records::records;

const COMPANY_SCHEMA: &str = "fungos.crm/company@1";
const ACCOUNT_SCHEMA: &str = "fungos.crm/account@1";
const ACCOUNT_COMPANY_SCHEMA: &str = "fungos.crm/account-company@1";
const CONTACT_SCHEMA: &str = "fungos.crm/contact@1";
const ASSIGNMENT_SCHEMA: &str = "fungos.crm/account-contact@1";
const PROJECTION_SCHEMA: &str = "fungos.crm/account-contact-projection@1";
const PROJECT_SCHEMA: &str = "fungos.crm/project@1";
const PROJECT_COMPANY_SCHEMA: &str = "fungos.crm/project-company@1";
const PROJECT_ACCOUNT_SCHEMA: &str = "fungos.crm/project-account@1";
const ACTIVITY_SCHEMA: &str = "fungos.crm/activity@1";
const OPPORTUNITY_SCHEMA: &str = "fungos.crm/opportunity@1";
const THREAD_SCHEMA: &str = "fungos.conversation/thread@1";
const MESSAGE_SCHEMA: &str = "fungos.conversation/message@1";
const LINK_SCHEMA: &str = "fungos.graph/link";

const DEFAULT_CRM: &str = "crm";
const CONTACTS: &str = "contacts";
const CALLS: &str = "calls";
const MESSAGES: &str = "messages";
const MAX_RECORDS: usize = 20_000;

#[derive(Clone)]
struct Row {
    namespace: String,
    id: String,
    schema: String,
    body: Value,
}

impl Row {
    fn field(&self, key: &str) -> &Value {
        self.body.get(key).unwrap_or(&Value::Null)
    }

    fn text(&self, key: &str) -> &str {
        self.field(key).as_str().unwrap_or_default()
    }
}

#[derive(Serialize)]
struct Event {
    id: String,
    kind: String,
    at_ms: i64,
    at: String,
    title: String,
    summary: String,
    actor: Option<String>,
    source: String,
    source_id: String,
    namespace: String,
    channel: Option<String>,
    thread_id: Option<String>,
    account_ids: Vec<String>,
    company_ids: Vec<String>,
    project_ids: Vec<String>,
    contact_tokens: Vec<String>,
    visibility: String,
}

#[derive(Default)]
struct TimelineFilter {
    account_id: Option<String>,
    project_id: Option<String>,
    contact_token: Option<String>,
}

struct Dataset {
    crm_namespace: String,
    crm: Vec<Row>,
    contacts: Vec<Row>,
    calls: Vec<Row>,
    messages: Vec<Row>,
}

struct Component;

export!(Component);

impl Guest for Component {
    fn run(args: Vec<String>) -> Result<(), String> {
        let (crm, args) = parse_crm(args)?;
        let action = args.first().map(String::as_str).unwrap_or("accounts");
        let data = Dataset::load(crm)?;
        match action {
            "accounts" => print_json("accounts", &data.accounts()?),
            "account" => {
                let id = args.get(1).ok_or_else(|| "usage: account <account-id>".to_string())?;
                print_json("account", &data.account(id)?);
            }
            "timeline" => {
                let id = args.get(1).ok_or_else(|| "usage: timeline <account-id> [limit]".to_string())?;
                let limit = args.get(2).and_then(|value| value.parse().ok()).unwrap_or(50);
                let events = data.timeline(
                    &TimelineFilter {
                        account_id: Some(id.clone()),
                        ..TimelineFilter::default()
                    },
                    limit,
                )?;
                print_json("timeline", &json!({ "account_id": id, "events": events }));
            }
            "project" => {
                let id = args.get(1).ok_or_else(|| "usage: project <project-id>".to_string())?;
                print_json("project", &data.project(id)?);
            }
            "pipeline" => print_json("pipeline", &data.pipeline(args.get(1).map(String::as_str))),
            "activity" if args.get(1).map(String::as_str) == Some("add") => {
                let value = add_activity(&data, &args)?;
                print_json("activity", &value);
            }
            "opportunity" if args.get(1).map(String::as_str) == Some("stage") => {
                let value = set_opportunity_stage(&data.crm_namespace, &args)?;
                print_json("opportunity", &value);
            }
            "contact" if args.get(1).map(String::as_str) == Some("link") => {
                let value = link_contact(&data, &args)?;
                print_json("contact", &value);
            }
            "contact" if args.get(1).map(String::as_str) == Some("share") => {
                let value = share_contact(&data, &args)?;
                print_json("contact", &value);
            }
            "contact" if args.get(1).map(String::as_str) == Some("unshare") => {
                let value = unshare_contact(&data.crm_namespace, &args)?;
                print_json("contact", &value);
            }
            "contacts" if args.get(1).map(String::as_str) == Some("shared") => {
                print_json("contacts", &data.shared_contacts());
            }
            "call" if args.get(1).map(String::as_str) == Some("add") => {
                let value = add_call(&data, &args)?;
                print_json("call", &value);
            }
            "email" if args.get(1).map(String::as_str) == Some("add") => {
                let value = add_email(&data, &args)?;
                print_json("email", &value);
            }
            "contact" => {
                let id = args.get(1).ok_or_else(|| "usage: contact <contact-id|contact-token>".to_string())?;
                print_json("contact", &data.contact(id)?);
            }
            "help" => print_json("help", &json!({
                "actions": [
                    "accounts",
                    "account <id>",
                    "contact <id|token>",
                    "project <id>",
                    "timeline <id> [limit]",
                    "pipeline",
                    "activity add <id> <account-id> <kind> <title> <occurred-at> [summary]",
                    "opportunity stage <opportunity-id> <stage>",
                    "contact link <account-id> <contact-id> [role]",
                    "contact share <account-id> <contact-id> [first-name]",
                    "contact unshare <account-id> <contact-id>",
                    "contacts shared",
                    "call add <id> <account-id> <title> <occurred-at> [summary]",
                    "email add <id> <thread-id> <account-id> <subject> <sender> <body> <occurred-at-ms>",
                ],
                "crm": DEFAULT_CRM,
                "usage": "--crm <namespace> <action> ...",
                "namespaces": [DEFAULT_CRM, CONTACTS, CALLS, MESSAGES],
            })),
            other => return Err(format!("unknown action {other:?}; use help")),
        }
        Ok(())
    }
}

impl Dataset {
    fn load(crm: String) -> Result<Self, String> {
        Ok(Self {
            crm_namespace: crm.clone(),
            crm: load_namespace(&crm, true)?,
            contacts: load_namespace(CONTACTS, false)?,
            calls: load_namespace(CALLS, false)?,
            messages: load_namespace(MESSAGES, false)?,
        })
    }

    fn all(&self) -> impl Iterator<Item = &Row> + '_ {
        self.crm
            .iter()
            .chain(self.contacts.iter())
            .chain(self.calls.iter())
            .chain(self.messages.iter())
    }

    fn rows<'a>(&'a self, schema: &'a str) -> impl Iterator<Item = &'a Row> + 'a {
        self.all().filter(move |row| row.schema == schema)
    }

    fn accounts(&self) -> Result<Value, String> {
        let companies = rows_by_id(self.rows(COMPANY_SCHEMA));
        let mut accounts = Vec::new();
        for account in self.rows(ACCOUNT_SCHEMA) {
            let companies = self
                .rows(ACCOUNT_COMPANY_SCHEMA)
                .filter(|link| link.text("account_id") == account.id)
                .filter_map(|link| companies.get(link.text("company_id")))
                .map(|company| json!({
                    "id": company.id,
                    "name": company.text("name"),
                    "roles": self
                        .rows(ACCOUNT_COMPANY_SCHEMA)
                        .find(|link| link.text("account_id") == account.id
                            && link.text("company_id") == company.id)
                        .map(|link| link.field("roles").clone())
                        .unwrap_or(Value::Null),
                }))
                .collect::<Vec<_>>();
            accounts.push(json!({
                "id": account.id,
                "name": account.text("name"),
                "domains": account.field("domains"),
                "industry": account.text("industry"),
                "owner": account.text("owner"),
                "companies": companies,
            }));
        }
        accounts.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        Ok(json!({ "accounts": accounts }))
    }

    fn account(&self, id: &str) -> Result<Value, String> {
        let account = self
            .rows(ACCOUNT_SCHEMA)
            .find(|row| row.id == id)
            .ok_or_else(|| format!("account {id} not found"))?;
        let contacts = self.contacts_for(id);
        let projects = self.projects_for(id);
        let activities = self
            .rows(ACTIVITY_SCHEMA)
            .filter(|row| row.text("account_id") == id)
            .map(activity_json)
            .collect::<Vec<_>>();
        let opportunities = self
            .rows(OPPORTUNITY_SCHEMA)
            .filter(|row| row.text("account_id") == id)
            .map(opportunity_json)
            .collect::<Vec<_>>();
        let companies = self.company_links(id);
        let events = self.timeline(
            &TimelineFilter {
                account_id: Some(id.to_string()),
                ..TimelineFilter::default()
            },
            25,
        )?;
        Ok(json!({
            "id": account.id,
            "name": account.text("name"),
            "legal_name": account.text("legal_name"),
            "domains": account.field("domains"),
            "website": account.text("website"),
            "industry": account.text("industry"),
            "phone": account.text("phone"),
            "description": account.text("description"),
            "owner": account.text("owner"),
            "companies": companies,
            "contacts": contacts,
            "projects": projects,
            "activities": activities,
            "opportunities": opportunities,
            "timeline": events,
        }))
    }

    fn pipeline(&self, company_id: Option<&str>) -> Value {
        let accounts = rows_by_id(self.rows(ACCOUNT_SCHEMA));
        let mut stages: BTreeMap<String, (u64, u64, Vec<Value>)> = BTreeMap::new();
        for opportunity in self.rows(OPPORTUNITY_SCHEMA) {
            let stage = opportunity.text("stage");
            if stage.is_empty() {
                continue;
            }
            if let Some(company_id) = company_id
                && opportunity.text("company_id") != company_id
            {
                continue;
            }
            let amount = opportunity
                .field("amount")
                .as_f64()
                .unwrap_or(0.0)
                .max(0.0) as u64;
            let entry = stages.entry(stage.to_string()).or_insert((0, 0, Vec::new()));
            entry.0 += 1;
            entry.1 += amount;
            entry.2.push(json!({
                "id": opportunity.id,
                "name": opportunity.text("name"),
                "account_id": opportunity.text("account_id"),
                "account_name": accounts
                    .get(opportunity.text("account_id"))
                    .map(|account| account.text("name"))
                    .unwrap_or(""),
                "amount": amount,
                "close_date": opportunity.text("close_date"),
                "owner": opportunity.text("owner"),
                "company_id": opportunity.text("company_id"),
            }));
        }
        let stages = stages
            .into_iter()
            .map(|(stage, (count, amount, opportunities))| json!({
                "stage": stage,
                "count": count,
                "amount": amount,
                "opportunities": opportunities,
            }))
            .collect::<Vec<_>>();
        json!({ "stages": stages })
    }

    fn contact(&self, id_or_token: &str) -> Result<Value, String> {
        let contact = self
            .rows(CONTACT_SCHEMA)
            .find(|row| row.id == id_or_token || row.text("contact_token") == id_or_token)
            .ok_or_else(|| format!("contact {id_or_token} not found"))?;
        let token = contact.text("contact_token");
        let assignments = self
            .rows(ASSIGNMENT_SCHEMA)
            .filter(|link| link.text("contact_id") == contact.id || link.text("contact_token") == token)
            .collect::<Vec<_>>();
        let accounts = rows_by_id(self.rows(ACCOUNT_SCHEMA));
        let account_values = assignments
            .iter()
            .filter_map(|link| accounts.get(link.text("account_id")))
            .map(|account| {
                json!({
                    "id": account.id,
                    "name": account.text("name"),
                    "domains": account.field("domains"),
                })
            })
            .collect::<Vec<_>>();
        let activities = self
            .rows(ACTIVITY_SCHEMA)
            .filter(|row| row.text("contact_token") == token)
            .map(activity_json)
            .collect::<Vec<_>>();
        let events = self.timeline(
            &TimelineFilter {
                contact_token: Some(token.to_string()),
                ..TimelineFilter::default()
            },
            50,
        )?;
        Ok(json!({
            "id": contact.id,
            "name": contact.text("name"),
            "first_name": contact.text("first_name"),
            "last_name": contact.text("last_name"),
            "email": contact.text("email"),
            "phone": contact.text("phone"),
            "title": contact.text("title"),
            "contact_token": token,
            "accounts": account_values,
            "assignments": assignments.iter().map(|link| json!({
                "account_id": link.text("account_id"),
                "role": link.text("role"),
                "status": link.text("status"),
            })).collect::<Vec<_>>(),
            "activities": activities,
            "timeline": events,
        }))
    }

    fn project(&self, id: &str) -> Result<Value, String> {
        let project = self
            .rows(PROJECT_SCHEMA)
            .find(|row| row.id == id)
            .ok_or_else(|| format!("project {id} not found"))?;
        let company = self
            .rows(COMPANY_SCHEMA)
            .find(|row| row.id == project.text("company_id"));
        let accounts = rows_by_id(self.rows(ACCOUNT_SCHEMA));
        let project_accounts = self
            .rows(PROJECT_ACCOUNT_SCHEMA)
            .filter(|link| link.text("project_id") == id)
            .map(|link| {
                let account = accounts.get(link.text("account_id"));
                json!({
                    "id": link.text("account_id"),
                    "name": account.map(|account| account.text("name")).unwrap_or(""),
                    "role": link.text("role"),
                    "primary": link.field("primary"),
                })
            })
            .collect::<Vec<_>>();
        let participants = self
            .rows(PROJECT_COMPANY_SCHEMA)
            .filter(|link| link.text("project_id") == id)
            .map(|link| json!({
                "company_id": link.text("company_id"),
                "role": link.text("role"),
            }))
            .collect::<Vec<_>>();
        let activities = self
            .rows(ACTIVITY_SCHEMA)
            .filter(|row| row.text("project_id") == id)
            .map(activity_json)
            .collect::<Vec<_>>();
        let opportunities = self
            .rows(OPPORTUNITY_SCHEMA)
            .filter(|row| row.text("project_id") == id)
            .map(opportunity_json)
            .collect::<Vec<_>>();
        let events = self.timeline(
            &TimelineFilter {
                project_id: Some(id.to_string()),
                ..TimelineFilter::default()
            },
            50,
        )?;
        Ok(json!({
            "id": project.id,
            "name": project.text("name"),
            "kind": project.text("kind"),
            "status": project.text("status"),
            "summary": project.text("summary"),
            "start_at": project.text("start_at"),
            "due_at": project.text("due_at"),
            "visibility": project.text("visibility"),
            "company": company.map(|company| json!({
                "id": company.id,
                "name": company.text("name"),
            })),
            "accounts": project_accounts,
            "participants": participants,
            "activities": activities,
            "opportunities": opportunities,
            "timeline": events,
        }))
    }

    fn shared_contacts(&self) -> Value {
        let accounts = rows_by_id(self.rows(ACCOUNT_SCHEMA));
        let contacts = self
            .rows(PROJECTION_SCHEMA)
            .map(|projection| json!({
                "contact_token": projection.text("contact_token"),
                "first_name": projection.text("first_name"),
                "account_id": projection.text("account_id"),
                "account_name": accounts
                    .get(projection.text("account_id"))
                    .map(|account| account.text("name"))
                    .unwrap_or(""),
                "shared": true,
            }))
            .collect::<Vec<_>>();
        json!({ "contacts": contacts })
    }

    fn contacts_for(&self, account_id: &str) -> Vec<Value> {
        let contacts = rows_by_id(self.rows(CONTACT_SCHEMA));
        let projections = self.rows(PROJECTION_SCHEMA).collect::<Vec<_>>();
        self.rows(ASSIGNMENT_SCHEMA)
            .filter(|assignment| assignment.text("account_id") == account_id)
            .map(|assignment| {
                let contact_id = assignment.text("contact_id");
                let token = assignment.text("contact_token");
                let contact = contacts.get(contact_id);
                let projection = projections
                    .iter()
                    .find(|projection| projection.text("contact_token") == token);
                if let Some(contact) = contact {
                    json!({
                        "id": contact.id,
                        "name": contact.text("name"),
                        "first_name": contact.text("first_name"),
                        "email": contact.text("email"),
                        "phone": contact.text("phone"),
                        "title": contact.text("title"),
                        "contact_token": token,
                        "role": assignment.text("role"),
                        "shared": projection.is_some(),
                    })
                } else if let Some(projection) = projection {
                    json!({
                        "id": contact_id,
                        "name": projection.text("first_name"),
                        "first_name": projection.text("first_name"),
                        "contact_token": token,
                        "role": assignment.text("role"),
                        "shared": true,
                    })
                } else {
                    json!({
                        "id": contact_id,
                        "name": token,
                        "contact_token": token,
                        "role": assignment.text("role"),
                        "shared": false,
                    })
                }
            })
            .collect()
    }

    fn projects_for(&self, account_id: &str) -> Vec<Value> {
        let projects = rows_by_id(self.rows(PROJECT_SCHEMA));
        self.rows(PROJECT_ACCOUNT_SCHEMA)
            .filter(|link| link.text("account_id") == account_id)
            .filter_map(|link| projects.get(link.text("project_id")))
            .map(|project| {
                let mut value = project_json(project);
                value["role"] = self
                    .rows(PROJECT_ACCOUNT_SCHEMA)
                    .find(|link| link.text("project_id") == project.id
                        && link.text("account_id") == account_id)
                    .map(|link| link.field("role").clone())
                    .unwrap_or(Value::Null);
                value
            })
            .collect()
    }

    fn company_links(&self, account_id: &str) -> Vec<Value> {
        let companies = rows_by_id(self.rows(COMPANY_SCHEMA));
        self.rows(ACCOUNT_COMPANY_SCHEMA)
            .filter(|link| link.text("account_id") == account_id)
            .map(|link| {
                let company = companies.get(link.text("company_id"));
                json!({
                    "company_id": link.text("company_id"),
                    "company_name": company.map(|company| company.text("name")).unwrap_or(""),
                    "roles": link.field("roles"),
                    "status": link.text("status"),
                    "owner": link.text("owner"),
                })
            })
            .collect()
    }

    fn timeline(&self, filter: &TimelineFilter, limit: usize) -> Result<Vec<Event>, String> {
        let accounts = rows_by_id(self.rows(ACCOUNT_SCHEMA));
        let contacts = rows_by_id(self.rows(CONTACT_SCHEMA));
        let projects = rows_by_id(self.rows(PROJECT_SCHEMA));
        let threads = rows_by_id(self.rows(THREAD_SCHEMA));
        let mut companies_by_account: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for link in self.rows(ACCOUNT_COMPANY_SCHEMA) {
            companies_by_account
                .entry(link.text("account_id").to_string())
                .or_default()
                .insert(link.text("company_id").to_string());
        }

        let mut targets_by_source: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for link in self.rows(LINK_SCHEMA) {
            if link.text("rel") != "mentions" {
                continue;
            }
            let Some((_, source_id)) = link.text("from").split_once('#') else {
                continue;
            };
            targets_by_source
                .entry((link.namespace.clone(), source_id.to_string()))
                .or_default()
                .push(link.text("to").to_string());
        }

        let mut events = Vec::new();
        for row in self.rows(ACTIVITY_SCHEMA) {
            let at_ms = parse_time_ms(row.text("occurred_at"));
            events.push(Event {
                id: format!("activity:{}:{}", row.namespace, row.id),
                kind: row.text("kind").to_string(),
                at_ms,
                at: iso_from_ms(at_ms),
                title: row.text("name").to_string(),
                summary: row.text("description").to_string(),
                actor: nonempty(row.text("owner")),
                source: ACTIVITY_SCHEMA.to_string(),
                source_id: row.id.clone(),
                namespace: row.namespace.clone(),
                channel: None,
                thread_id: None,
                account_ids: one(row.text("account_id")),
                company_ids: one(row.text("company_id")),
                project_ids: one(row.text("project_id")),
                contact_tokens: one(row.text("contact_token")),
                visibility: default_team(row.text("visibility")),
            });
        }

        for message in self.rows(MESSAGE_SCHEMA) {
            let thread_id = message.text("thread_id");
            let thread = threads.get(thread_id);
            let visibility = thread
                .map(|thread| thread.text("visibility"))
                .unwrap_or("private");
            let mut targets = targets_by_source
                .get(&(message.namespace.clone(), message.id.clone()))
                .cloned()
                .unwrap_or_default();
            if let Some(mentions) = message.field("mentions").as_array() {
                for value in mentions {
                    let Some(value) = value.as_str() else {
                        continue;
                    };
                    if value.contains("@[") {
                        targets.extend(parse_mentions(value));
                    } else if value.contains(':') {
                        targets.push(value.to_string());
                    }
                }
            }
            if targets.is_empty() {
                continue;
            }
            let mut account_ids = BTreeSet::new();
            let mut company_ids = BTreeSet::new();
            let mut project_ids = BTreeSet::new();
            let mut contact_tokens = BTreeSet::new();
            for target in targets {
                let Some(entity) = target.rsplit_once('#').map(|(_, entity)| entity) else {
                    continue;
                };
                if accounts.contains_key(entity) {
                    account_ids.insert(entity.to_string());
                    if let Some(companies) = companies_by_account.get(entity) {
                        company_ids.extend(companies.iter().cloned());
                    }
                }
                if let Some(contact) = contacts.get(entity) {
                    let token = contact.text("contact_token");
                    if !token.is_empty() {
                        contact_tokens.insert(token.to_string());
                    }
                }
                if let Some(project) = projects.get(entity) {
                    project_ids.insert(entity.to_string());
                    let company = project.text("company_id");
                    if !company.is_empty() {
                        company_ids.insert(company.to_string());
                    }
                }
            }
            if let Some(account_id) = message.field("account_id").as_str()
                && !account_id.is_empty()
            {
                account_ids.insert(account_id.to_string());
                if let Some(companies) = companies_by_account.get(account_id) {
                    company_ids.extend(companies.iter().cloned());
                }
            }
            if account_ids.is_empty()
                && company_ids.is_empty()
                && project_ids.is_empty()
                && contact_tokens.is_empty()
            {
                continue;
            }
            let at_ms = message
                .field("occurred_at_ms")
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .unwrap_or(0);
            events.push(Event {
                id: format!("message:{}:{}", message.namespace, message.id),
                kind: "message".to_string(),
                at_ms,
                at: iso_from_ms(at_ms),
                title: thread
                    .map(|thread| format!("Message in {}", thread.text("title")))
                    .unwrap_or_else(|| "Message".to_string()),
                summary: message.text("body").to_string(),
                actor: nonempty(message.text("sender")),
                source: MESSAGE_SCHEMA.to_string(),
                source_id: message.id.clone(),
                namespace: message.namespace.clone(),
                channel: nonempty(message.text("channel")),
                thread_id: nonempty(thread_id),
                account_ids: account_ids.into_iter().collect(),
                company_ids: company_ids.into_iter().collect(),
                project_ids: project_ids.into_iter().collect(),
                contact_tokens: contact_tokens.into_iter().collect(),
                visibility: visibility.to_string(),
            });
        }

        if let Some(account_id) = &filter.account_id {
            events.retain(|event| event.account_ids.iter().any(|id| id == account_id));
        }
        if let Some(project_id) = &filter.project_id {
            events.retain(|event| event.project_ids.iter().any(|id| id == project_id));
        }
        if let Some(contact_token) = &filter.contact_token {
            events.retain(|event| event.contact_tokens.iter().any(|token| token == contact_token));
        }
        events.sort_by(|left, right| {
            right
                .at_ms
                .cmp(&left.at_ms)
                .then_with(|| right.id.cmp(&left.id))
        });
        events.truncate(limit.max(1));
        Ok(events)
    }
}

fn add_activity(data: &Dataset, args: &[String]) -> Result<Value, String> {
    let id = args.get(2).ok_or_else(|| {
        "usage: activity add <id> <account-id> <kind> <title> <occurred-at> [summary]".to_string()
    })?;
    let account_id = args.get(3).ok_or_else(|| "activity add: missing account id".to_string())?;
    let kind = args.get(4).ok_or_else(|| "activity add: missing kind".to_string())?;
    let title = args.get(5).ok_or_else(|| "activity add: missing title".to_string())?;
    let occurred_at = args
        .get(6)
        .ok_or_else(|| "activity add: missing occurred-at".to_string())?;
    let summary = args.iter().skip(7).cloned().collect::<Vec<_>>().join(" ");
    if data.rows(ACCOUNT_SCHEMA).all(|account| account.id != *account_id) {
        return Err(format!("account {account_id} not found"));
    }
    let company_id = data
        .rows(ACCOUNT_COMPANY_SCHEMA)
        .find(|link| link.text("account_id") == account_id)
        .map(|link| link.text("company_id"))
        .unwrap_or_default();
    let body = json!({
        "kind": kind,
        "name": title,
        "occurred_at": occurred_at,
        "description": summary,
        "account_id": account_id,
        "company_id": company_id,
        "visibility": "team",
        "source": "spore",
        "source_id": id,
    });
    let stored = put_record(&data.crm_namespace, id, ACTIVITY_SCHEMA, body)?;
    Ok(json!({
        "id": id,
        "schema": ACTIVITY_SCHEMA,
        "account_id": account_id,
        "kind": kind,
        "title": title,
        "occurred_at": occurred_at,
        "revision": stored.revision,
    }))
}

fn set_opportunity_stage(crm: &str, args: &[String]) -> Result<Value, String> {
    let id = args.get(2).ok_or_else(|| {
        "usage: opportunity stage <opportunity-id> <stage>".to_string()
    })?;
    let stage = args.get(3).ok_or_else(|| "opportunity stage: missing stage".to_string())?;
    let entry = records::get(&key(crm, id)).map_err(|error| format!("{error:?}"))?;
    if entry.schema != OPPORTUNITY_SCHEMA {
        return Err(format!("{id} is {}, not an opportunity", entry.schema));
    }
    let mut body: Value =
        serde_json::from_slice(&entry.body).map_err(|error| format!("decode {id}: {error}"))?;
    body["stage"] = Value::String(stage.clone());
    let stored = put_record(crm, id, OPPORTUNITY_SCHEMA, body)?;
    Ok(json!({
        "id": id,
        "stage": stage,
        "revision": stored.revision,
    }))
}

fn link_contact(data: &Dataset, args: &[String]) -> Result<Value, String> {
    let account_id = args
        .get(2)
        .ok_or_else(|| "usage: contact link <account-id> <contact-id> [role]".to_string())?;
    let contact_id = args
        .get(3)
        .ok_or_else(|| "contact link: missing contact id".to_string())?;
    let role = args.get(4).cloned().unwrap_or_default();
    if data.rows(ACCOUNT_SCHEMA).all(|account| account.id != *account_id) {
        return Err(format!("account {account_id} not found"));
    }
    let contact = records::get(&key(CONTACTS, contact_id))
        .map_err(|error| format!("contact {contact_id}: {error:?}"))?;
    if contact.schema != CONTACT_SCHEMA {
        return Err(format!("{contact_id} is {}, not a contact", contact.schema));
    }
    let body: Value = serde_json::from_slice(&contact.body)
        .map_err(|error| format!("decode contact {contact_id}: {error}"))?;
    let token = body
        .get("contact_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| format!("contact {contact_id} has no contact_token"))?;
    let assignment_id = format!("ac-{account_id}-{contact_id}");
    let stored = put_record(
        &data.crm_namespace,
        &assignment_id,
        ASSIGNMENT_SCHEMA,
        json!({
            "account_id": account_id,
            "contact_id": contact_id,
            "contact_token": token,
            "role": role,
            "status": "active",
            "source": "spore",
        }),
    )?;
    Ok(json!({
        "id": assignment_id,
        "account_id": account_id,
        "contact_id": contact_id,
        "contact_token": token,
        "role": role,
        "revision": stored.revision,
    }))
}

fn share_contact(data: &Dataset, args: &[String]) -> Result<Value, String> {
    let account_id = args
        .get(2)
        .ok_or_else(|| "usage: contact share <account-id> <contact-id> [first-name]".to_string())?;
    let contact_id = args
        .get(3)
        .ok_or_else(|| "contact share: missing contact id".to_string())?;
    if data.rows(ACCOUNT_SCHEMA).all(|account| account.id != *account_id) {
        return Err(format!("account {account_id} not found"));
    }
    let contact = data
        .rows(CONTACT_SCHEMA)
        .find(|row| row.id == *contact_id || row.text("contact_token") == *contact_id)
        .ok_or_else(|| format!("contact {contact_id} not found"))?;
    let token = contact.text("contact_token");
    if token.is_empty() {
        return Err(format!("contact {contact_id} has no contact_token"));
    }
    let first_name = args
        .get(4)
        .cloned()
        .filter(|name| !name.is_empty())
        .or_else(|| nonempty(contact.text("first_name")))
        .unwrap_or_else(|| token.to_string());
    let projection_id = format!("proj-{account_id}-{}", contact.id);
    let stored = put_record(
        &data.crm_namespace,
        &projection_id,
        PROJECTION_SCHEMA,
        json!({
            "contact_token": token,
            "account_id": account_id,
            "first_name": first_name,
            "hidden": false,
        }),
    )?;
    Ok(json!({
        "id": projection_id,
        "account_id": account_id,
        "contact_token": token,
        "first_name": first_name,
        "revision": stored.revision,
    }))
}

fn unshare_contact(crm: &str, args: &[String]) -> Result<Value, String> {
    let account_id = args
        .get(2)
        .ok_or_else(|| "usage: contact unshare <account-id> <contact-id>".to_string())?;
    let contact_id = args
        .get(3)
        .ok_or_else(|| "contact unshare: missing contact id".to_string())?;
    let projection_id = format!("proj-{account_id}-{contact_id}");
    records::delete(&key(crm, &projection_id))
        .map_err(|error| format!("unshare {projection_id}: {error:?}"))?;
    Ok(json!({ "id": projection_id, "shared": false }))
}

fn add_call(data: &Dataset, args: &[String]) -> Result<Value, String> {
    let id = args.get(2).ok_or_else(|| {
        "usage: call add <id> <account-id> <title> <occurred-at> [summary]".to_string()
    })?;
    let account_id = args.get(3).ok_or_else(|| "call add: missing account id".to_string())?;
    let title = args.get(4).ok_or_else(|| "call add: missing title".to_string())?;
    let occurred_at = args
        .get(5)
        .ok_or_else(|| "call add: missing occurred-at".to_string())?;
    let summary = args.iter().skip(6).cloned().collect::<Vec<_>>().join(" ");
    if data.rows(ACCOUNT_SCHEMA).all(|account| account.id != *account_id) {
        return Err(format!("account {account_id} not found"));
    }
    let stored = put_record(
        CALLS,
        id,
        ACTIVITY_SCHEMA,
        json!({
            "kind": "call",
            "name": title,
            "occurred_at": occurred_at,
            "description": summary,
            "account_id": account_id,
            "visibility": "private",
            "source": "spore",
            "source_id": id,
        }),
    )?;
    Ok(json!({
        "id": id,
        "namespace": CALLS,
        "account_id": account_id,
        "title": title,
        "occurred_at": occurred_at,
        "revision": stored.revision,
    }))
}

fn add_email(data: &Dataset, args: &[String]) -> Result<Value, String> {
    let id = args.get(2).ok_or_else(|| {
        "usage: email add <id> <thread-id> <account-id> <subject> <sender> <body> <occurred-at-ms>"
            .to_string()
    })?;
    let thread_id = args.get(3).ok_or_else(|| "email add: missing thread id".to_string())?;
    let account_id = args.get(4).ok_or_else(|| "email add: missing account id".to_string())?;
    let subject = args.get(5).ok_or_else(|| "email add: missing subject".to_string())?;
    let sender = args.get(6).ok_or_else(|| "email add: missing sender".to_string())?;
    let body = args.get(7).ok_or_else(|| "email add: missing body".to_string())?;
    let occurred_at_ms = args
        .get(8)
        .ok_or_else(|| "email add: missing occurred-at-ms".to_string())?
        .parse::<u64>()
        .map_err(|_| "email add: occurred-at-ms must be an integer".to_string())?;
    if data.rows(ACCOUNT_SCHEMA).all(|account| account.id != *account_id) {
        return Err(format!("account {account_id} not found"));
    }
    put_record(
        MESSAGES,
        thread_id,
        THREAD_SCHEMA,
        json!({
            "channel": "email",
            "external_id": thread_id,
            "title": subject,
            "participants": [sender],
            "visibility": "private",
            "first_at_ms": occurred_at_ms,
            "last_at_ms": occurred_at_ms,
        }),
    )?;
    let stored = put_record(
        MESSAGES,
        id,
        MESSAGE_SCHEMA,
        json!({
            "thread_id": thread_id,
            "channel": "email",
            "external_id": id,
            "direction": "inbound",
            "sender": sender,
            "body": body,
            "attachments": [],
            "mentions": [],
            "source_refs": [],
            "occurred_at_ms": occurred_at_ms,
            "edited_at_ms": null,
            "deleted": false,
            "account_id": account_id,
        }),
    )?;
    Ok(json!({
        "id": id,
        "thread_id": thread_id,
        "namespace": MESSAGES,
        "account_id": account_id,
        "subject": subject,
        "revision": stored.revision,
    }))
}

fn key(namespace: &str, id: &str) -> records::Key {
    records::Key {
        collection: namespace.to_string(),
        id: id.to_string(),
    }
}

fn put_record(
    namespace: &str,
    id: &str,
    schema: &str,
    body: Value,
) -> Result<records::Entry, String> {
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

fn parse_crm(args: Vec<String>) -> Result<(String, Vec<String>), String> {
    if let Some(first) = args.first() {
        if first == "--crm" {
            let name = args
                .get(1)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "--crm requires a namespace such as crm or crm:fpl".to_string())?;
            return Ok((name.clone(), args[2..].to_vec()));
        }
        if let Some(name) = first.strip_prefix("--crm=")
            && !name.is_empty()
        {
            return Ok((name.to_string(), args[1..].to_vec()));
        }
    }
    Ok((DEFAULT_CRM.to_string(), args))
}

fn load_namespace(name: &str, required: bool) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let page = match records::scan(name, cursor.as_deref(), 500) {
            Ok(page) => page,
            Err(records::Error::Denied) if !required => return Ok(rows),
            Err(error) => return Err(format!("records {name}: {error:?}")),
        };
        for entry in page.entries {
            if entry.deleted {
                continue;
            }
            let body = serde_json::from_slice::<Value>(&entry.body)
                .map_err(|error| format!("decode {}: {error}", entry.key.id))?;
            rows.push(Row {
                namespace: name.to_string(),
                id: entry.key.id,
                schema: entry.schema,
                body,
            });
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

fn rows_by_id<'a>(rows: impl Iterator<Item = &'a Row>) -> BTreeMap<String, &'a Row> {
    rows.map(|row| (row.id.clone(), row)).collect()
}

fn activity_json(row: &Row) -> Value {
    json!({
        "id": row.id,
        "kind": row.text("kind"),
        "name": row.text("name"),
        "occurred_at": row.text("occurred_at"),
        "description": row.text("description"),
        "account_id": row.text("account_id"),
        "project_id": row.text("project_id"),
        "contact_token": row.text("contact_token"),
        "visibility": row.text("visibility"),
    })
}

fn opportunity_json(row: &Row) -> Value {
    json!({
        "id": row.id,
        "name": row.text("name"),
        "stage": row.text("stage"),
        "amount": row.field("amount"),
        "close_date": row.text("close_date"),
        "account_id": row.text("account_id"),
    })
}

fn project_json(row: &Row) -> Value {
    json!({
        "id": row.id,
        "name": row.text("name"),
        "kind": row.text("kind"),
        "status": row.text("status"),
        "company_id": row.text("company_id"),
        "summary": row.text("summary"),
        "due_at": row.text("due_at"),
        "visibility": row.text("visibility"),
    })
}

fn parse_mentions(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("@[") {
        rest = &rest[start + 2..];
        let Some(label_end) = rest.find("](") else { break };
        let target_start = label_end + 2;
        let Some(target_end) = rest[target_start..].find(')') else { break };
        let target = rest[target_start..target_start + target_end].trim();
        if !target.is_empty() {
            out.push(target.to_string());
        }
        rest = &rest[target_start + target_end + 1..];
    }
    out
}

fn parse_time_ms(value: &str) -> i64 {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.timestamp_millis())
        .unwrap_or(0)
}

fn iso_from_ms(value: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(value)
        .map(|value| value.to_rfc3339())
        .unwrap_or_default()
}

fn one(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        vec![value.to_string()]
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn default_team(value: &str) -> String {
    if value.is_empty() {
        "team".to_string()
    } else {
        value.to_string()
    }
}

fn print_json(label: &str, value: &Value) {
    println!(
        "{label}: {}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}
