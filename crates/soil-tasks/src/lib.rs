//! Task records with mention persistence.
//!
//! Tasks are ordinary `fungos-records` entries. A task body may contain
//! `@[label](locator)` mentions; creating or updating it reconciles the
//! matching `mentions` graph links.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fungos_records::{Record, Store};
use serde::Serialize;
use serde_json::{Value, json};
use soil_mentions::{MentionLink, backlinks, links_for_source, sync};

pub const TASK_SCHEMA: &str = "fungos.task/task@1";
pub const TASK_VERSION: i64 = 1;
pub const TASK_STATUSES: &[&str] = &["todo", "doing", "blocked", "done"];

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub owner: Option<String>,
    pub due_at: Option<String>,
    pub project_id: Option<String>,
    pub account_id: Option<String>,
    pub opportunity_id: Option<String>,
    pub contact_token: Option<String>,
    pub source_activity_id: Option<String>,
    pub visibility: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub id: Option<String>,
    pub title: String,
    pub body: String,
    pub owner: Option<String>,
    pub due_at: Option<String>,
    pub project_id: Option<String>,
    pub account_id: Option<String>,
    pub opportunity_id: Option<String>,
    pub contact_token: Option<String>,
    pub source_activity_id: Option<String>,
    pub visibility: Option<String>,
}

/// Create a task and reconcile its mention links.
pub fn create_task(store: &mut Store, book: &str, task: NewTask) -> Result<Task> {
    if task.title.trim().is_empty() {
        anyhow::bail!("task title is required");
    }
    let id = task.id.unwrap_or_else(next_id);
    let body = task.body.clone();
    let now = now_ms();
    let visibility = task.visibility.unwrap_or_else(|| "private".into());
    store.put_record(
        book,
        &id,
        TASK_SCHEMA,
        TASK_VERSION,
        json!({
            "title": task.title,
            "body": body,
            "status": "todo",
            "owner": task.owner,
            "due_at": task.due_at,
            "project_id": task.project_id,
            "account_id": task.account_id,
            "opportunity_id": task.opportunity_id,
            "contact_token": task.contact_token,
            "source_activity_id": task.source_activity_id,
            "visibility": visibility,
            "created_at_ms": now,
            "updated_at_ms": now,
        }),
    )?;
    sync(store, book, &id, &body)?;
    get_task(store, book, &id)
}

/// Read one task.
pub fn get_task(store: &Store, book: &str, id: &str) -> Result<Task> {
    let record = store
        .get(book, id)
        .with_context(|| format!("task {id} not found"))?;
    task_from_record(record)
}

/// List tasks, newest first, with optional status and owner filters.
pub fn list_tasks(
    store: &Store,
    book: &str,
    status: Option<&str>,
    owner: Option<&str>,
) -> Result<Vec<Task>> {
    let mut tasks = store
        .list_schema(book, TASK_SCHEMA, 10_000)
        .with_context(|| format!("read tasks in {book}"))?
        .into_iter()
        .map(task_from_record)
        .collect::<Result<Vec<_>>>()?;
    if let Some(status) = status {
        tasks.retain(|task| task.status == status);
    }
    if let Some(owner) = owner {
        tasks.retain(|task| task.owner.as_deref() == Some(owner));
    }
    tasks.sort_by(|left, right| {
        right
            .created_at_ms
            .cmp(&left.created_at_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(tasks)
}

/// Mark a task done.
pub fn complete_task(store: &mut Store, book: &str, id: &str) -> Result<Task> {
    let task = get_task(store, book, id)?;
    let mut fields = task_fields(&task);
    fields["status"] = Value::String("done".into());
    fields["updated_at_ms"] = json!(now_ms());
    store.put_record(book, id, TASK_SCHEMA, TASK_VERSION, fields)?;
    get_task(store, book, id)
}

/// Replace the task body and reconcile mention links.
pub fn update_task_body(store: &mut Store, book: &str, id: &str, body: &str) -> Result<Task> {
    let task = get_task(store, book, id)?;
    let mut fields = task_fields(&task);
    fields["body"] = Value::String(body.into());
    fields["updated_at_ms"] = json!(now_ms());
    store.put_record(book, id, TASK_SCHEMA, TASK_VERSION, fields)?;
    sync(store, book, id, body)?;
    get_task(store, book, id)
}

/// Outgoing mention links for a task.
pub fn task_mentions(store: &Store, book: &str, id: &str) -> Result<Vec<MentionLink>> {
    links_for_source(store, book, id)
}

/// Incoming mentions of a task from the given books.
pub fn task_backlinks(
    store: &Store,
    books: &[String],
    book: &str,
    id: &str,
) -> Result<Vec<MentionLink>> {
    backlinks(store, books, &soil_mentions::source_locator(book, id))
}

fn task_from_record(record: Record) -> Result<Task> {
    if record.schema_id != TASK_SCHEMA {
        anyhow::bail!(
            "record {} has schema {}, expected {TASK_SCHEMA}",
            record.entity_id,
            record.schema_id
        );
    }
    let fields = &record.fields;
    Ok(Task {
        id: record.entity_id,
        title: string_field(fields, "title").unwrap_or_default(),
        body: string_field(fields, "body").unwrap_or_default(),
        status: string_field(fields, "status").unwrap_or_else(|| "todo".into()),
        owner: string_field(fields, "owner"),
        due_at: string_field(fields, "due_at"),
        project_id: string_field(fields, "project_id"),
        account_id: string_field(fields, "account_id"),
        opportunity_id: string_field(fields, "opportunity_id"),
        contact_token: string_field(fields, "contact_token"),
        source_activity_id: string_field(fields, "source_activity_id"),
        visibility: string_field(fields, "visibility").unwrap_or_else(|| "private".into()),
        created_at_ms: u64_field(fields, "created_at_ms"),
        updated_at_ms: u64_field(fields, "updated_at_ms"),
    })
}

fn task_fields(task: &Task) -> Value {
    json!({
        "title": task.title,
        "body": task.body,
        "status": task.status,
        "owner": task.owner,
        "due_at": task.due_at,
        "project_id": task.project_id,
        "account_id": task.account_id,
        "opportunity_id": task.opportunity_id,
        "contact_token": task.contact_token,
        "source_activity_id": task.source_activity_id,
        "visibility": task.visibility,
        "created_at_ms": task.created_at_ms,
        "updated_at_ms": task.updated_at_ms,
    })
}

fn string_field(fields: &Value, key: &str) -> Option<String> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn u64_field(fields: &Value, key: &str) -> u64 {
    fields.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn next_id() -> String {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("task-{}-{seq}", now_ms())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &std::path::Path) -> Store {
        Store::open(root, &root.join("device_id")).unwrap()
    }

    #[test]
    fn creates_task_with_mention_links_and_backlinks() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let book = "personal/test/tasks";
        let task = create_task(
            &mut store,
            book,
            NewTask {
                id: Some("task-1".into()),
                title: "Send pilot brief".into(),
                body: "Ask @[Ada](record:personal/crm#c-ada) for SSO notes.".into(),
                owner: Some("Avery".into()),
                ..NewTask::default()
            },
        )
        .unwrap();
        assert_eq!(task.id, "task-1");
        assert_eq!(task.status, "todo");

        let mentions = task_mentions(&store, book, "task-1").unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].target, "record:personal/crm#c-ada");

        let backlinks =
            task_backlinks(&store, &[book.to_string()], "personal/crm", "c-ada").unwrap();
        assert_eq!(backlinks.len(), 1);
        assert_eq!(backlinks[0].book, book);
    }

    #[test]
    fn updates_body_and_removes_stale_mentions() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let book = "personal/test/tasks";
        create_task(
            &mut store,
            book,
            NewTask {
                id: Some("task-1".into()),
                title: "Follow up".into(),
                body: "@[Ada](record:personal/crm#c-ada)".into(),
                ..NewTask::default()
            },
        )
        .unwrap();
        update_task_body(&mut store, book, "task-1", "No mention now.").unwrap();
        assert!(task_mentions(&store, book, "task-1").unwrap().is_empty());
    }

    #[test]
    fn lists_and_completes_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let book = "personal/test/tasks";
        create_task(
            &mut store,
            book,
            NewTask {
                id: Some("task-1".into()),
                title: "One".into(),
                ..NewTask::default()
            },
        )
        .unwrap();
        create_task(
            &mut store,
            book,
            NewTask {
                id: Some("task-2".into()),
                title: "Two".into(),
                owner: Some("Avery".into()),
                ..NewTask::default()
            },
        )
        .unwrap();
        assert_eq!(list_tasks(&store, book, None, None).unwrap().len(), 2);
        assert_eq!(
            list_tasks(&store, book, None, Some("Avery")).unwrap().len(),
            1
        );
        let done = complete_task(&mut store, book, "task-1").unwrap();
        assert_eq!(done.status, "done");
        assert_eq!(
            list_tasks(&store, book, Some("done"), None).unwrap().len(),
            1
        );
    }
}
