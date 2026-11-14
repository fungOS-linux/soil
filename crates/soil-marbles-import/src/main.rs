//! Import a local Marbles SQLite store into a fungos-records book.
//!
//! The source is opened read-only. The target is one book/collection named by
//! the caller; entity ids are `<type>/<json-key>` so the wasm `mb` SoilEngine
//! can read the same book.
//!
//! Consolidating two machines means running this twice into one book. That only
//! works if a second run is neither destructive nor order-dependent, so:
//!
//! - `--newest-wins` keeps whichever of two revisions of an issue has the later
//!   `updated_at`. Without it the last import silently overwrites the first,
//!   which is how the newer of two edits gets lost by running them in the wrong
//!   order.
//! - Event ids continue from the highest one already in the book. Deriving them
//!   from a running counter would give the second import the same ids as the
//!   first, so two different events would share one `seq` and `mb history` would
//!   interleave them.
//!
//! History is **opt-in** (`--history`). A fleet-sized store carries hundreds of
//! thousands of audit rows — neo's is 404,000, of which 393,000 are from the last
//! month — and `mb` seeds its event counter by scanning the collection on every
//! invocation. Loading the archive would therefore make every command pay for it.
//! The SQLite file is the archive; `mb history` covers what happens from here on.

use std::{path::Path, process::ExitCode};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mb-records-import: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// What one run did, so a two-machine consolidation can be reviewed instead of
/// trusted.
#[derive(Default)]
struct Report {
    projects: usize,
    issues: usize,
    /// Issues the target already had a newer-or-equal revision of.
    skipped_issues: usize,
    deps: usize,
    events: usize,
}

impl Report {
    fn written(&self) -> usize {
        self.projects + self.issues + self.deps + self.events
    }
}

fn run() -> Result<()> {
    let mut from = None;
    let mut root = None;
    let mut book = None;
    let mut dry_run = false;
    let mut newest_wins = false;
    let mut history = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                from = Some(args.get(i + 1).context("--from PATH")?.clone());
                i += 2;
            }
            "--root" => {
                root = Some(args.get(i + 1).context("--root PATH")?.clone());
                i += 2;
            }
            "--book" => {
                book = Some(args.get(i + 1).context("--book ID")?.clone());
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--newest-wins" => {
                newest_wins = true;
                i += 1;
            }
            "--history" => {
                history = true;
                i += 1;
            }
            "-h" | "--help" => {
                println!(
                    "usage: mb-records-import --from SQLITE --root RECORDS --book ID \\\n                     [--newest-wins] [--history] [--dry-run]"
                );
                return Ok(());
            }
            other => bail!("unknown option {other}"),
        }
    }
    let from = from.context("--from PATH")?;
    let root = root.context("--root PATH")?;
    let book = book.context("--book ID")?;
    let source = Connection::open_with_flags(
        Path::new(&from),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open {from} read-only"))?;
    let counts = counts(&source)?;
    println!(
        "source projects={} issues={} deps={} history={}",
        counts.0, counts.1, counts.2, counts.3
    );
    if dry_run {
        println!("dry-run: no records written; the merge plan needs the target open");
        return Ok(());
    }
    let root_path = Path::new(&root);
    let mut target = fungos_records::Store::open(root_path, &root_path.join("device_id"))
        .context("open fungos-records")?;
    let mut report = Report::default();
    write_projects(&source, &mut target, &book, &mut report)?;
    write_issues(&source, &mut target, &book, newest_wins, &mut report)?;
    write_deps(&source, &mut target, &book, &mut report)?;
    if history {
        write_history(&source, &mut target, &book, &mut report)?;
    } else {
        println!(
            "skipped {} history rows; pass --history to load them (they are read on every \
             `mb` call, so a fleet-sized archive makes commands slow)",
            counts.3
        );
    }
    println!(
        "wrote {} records into book {book}: projects={} issues={} deps={} events={}",
        report.written(),
        report.projects,
        report.issues,
        report.deps,
        report.events
    );
    if report.skipped_issues > 0 {
        println!(
            "kept the newer target revision for {} issues (--newest-wins)",
            report.skipped_issues
        );
    }
    Ok(())
}

fn counts(source: &Connection) -> Result<(i64, i64, i64, i64)> {
    Ok((
        source.query_row("SELECT count(*) FROM project", [], |row| row.get(0))?,
        source.query_row("SELECT count(*) FROM issue", [], |row| row.get(0))?,
        source.query_row("SELECT count(*) FROM dep", [], |row| row.get(0))?,
        source.query_row("SELECT count(*) FROM history", [], |row| row.get(0))?,
    ))
}

fn entity(kind: &str, key: &str) -> String {
    format!("{kind}/{}", serde_json::to_string(&[key]).expect("key"))
}

fn write_projects(
    source: &Connection,
    target: &mut fungos_records::Store,
    book: &str,
    report: &mut Report,
) -> Result<()> {
    let mut stmt =
        source.prepare("SELECT slug,root,prefix,created_at FROM project ORDER BY slug")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let slug: String = row.get(0)?;
        let body = json!({
            "slug": slug,
            "root": row.get::<_, String>(1)?,
            "prefix": row.get::<_, String>(2)?,
            "created_at": row.get::<_, i64>(3)?,
        });
        target.put_record(
            book,
            &entity("project", &slug),
            "marbles.project@1",
            1,
            body,
        )?;
        report.projects += 1;
    }
    Ok(())
}

/// The issue body already on the target, if any.
///
/// The body carries `updated_at`, which is the field the two machines disagree
/// on; the record's own revision is a wall-clock HLC for when *this* importer
/// wrote it and says nothing about which source was newer.
fn existing_updated_at(
    target: &fungos_records::Store,
    book: &str,
    id: &str,
) -> Result<Option<i64>> {
    let Some(record) = target.get_optional(book, id)? else {
        return Ok(None);
    };
    Ok(Some(
        record.fields["updated_at"].as_i64().unwrap_or_default(),
    ))
}

fn write_issues(
    source: &Connection,
    target: &mut fungos_records::Store,
    book: &str,
    newest_wins: bool,
    report: &mut Report,
) -> Result<()> {
    let mut stmt = source.prepare(
        "SELECT id,project,title,description,status,issue_type,priority,parent,labels_json,
                assignee,actor_kind,expires_at,available_at,created_at,updated_at,closed_at,
                evidence_json,metadata_json,external_ref
         FROM issue ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let updated_at: i64 = row.get(14)?;
        let entity_id = entity("issue", &id);
        if newest_wins
            && let Some(existing) = existing_updated_at(target, book, &entity_id)?
            && existing >= updated_at
        {
            report.skipped_issues += 1;
            continue;
        }
        let labels: Vec<String> = serde_json::from_str(&row.get::<_, String>(8)?)?;
        let evidence: Value = serde_json::from_str(&row.get::<_, String>(16)?)?;
        let metadata: Value = serde_json::from_str(&row.get::<_, String>(17)?)?;
        let body = json!({
            "id": id,
            "project": row.get::<_, String>(1)?,
            "title": row.get::<_, String>(2)?,
            "description": row.get::<_, String>(3)?,
            "status": row.get::<_, String>(4)?,
            "issue_type": row.get::<_, String>(5)?,
            "priority": row.get::<_, i64>(6)?,
            "parent": row.get::<_, Option<String>>(7)?,
            "labels": labels,
            "assignee": row.get::<_, Option<String>>(9)?,
            "actor_kind": row.get::<_, Option<String>>(10)?,
            "expires_at": row.get::<_, Option<i64>>(11)?,
            "available_at": row.get::<_, Option<i64>>(12)?,
            "created_at": row.get::<_, i64>(13)?,
            "updated_at": updated_at,
            "closed_at": row.get::<_, Option<i64>>(15)?,
            "evidence": evidence,
            "metadata": metadata,
            "external_ref": row.get::<_, Option<String>>(18)?,
        });
        target.put_record(book, &entity_id, "marbles.issue@1", 1, body)?;
        report.issues += 1;
    }
    Ok(())
}

fn write_deps(
    source: &Connection,
    target: &mut fungos_records::Store,
    book: &str,
    report: &mut Report,
) -> Result<()> {
    let mut stmt =
        source.prepare("SELECT issue_id,depends_on FROM dep ORDER BY issue_id,depends_on")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let child: String = row.get(0)?;
        let parent: String = row.get(1)?;
        let key = format!("{child}\u{1f}{parent}");
        let entity_id = entity("dependency", &key);
        // An edge is a fact, not a value: `child depends_on parent` cannot
        // become something else. Skipping one that is already there is what
        // makes a re-run write nothing instead of appending the same 1,559
        // edges again.
        if target.get_optional(book, &entity_id)?.is_some() {
            continue;
        }
        target.put_record(
            book,
            &entity_id,
            "marbles.dependency@1",
            1,
            json!({"child": child, "parent": parent}),
        )?;
        report.deps += 1;
    }
    Ok(())
}

/// The highest event id already in the book, so a second import continues the
/// sequence instead of reusing it.
fn next_event_seq(target: &mut fungos_records::Store, book: &str) -> Result<i64> {
    const MAX_PAGE: usize = 512;
    let mut cursor = None;
    let mut max = 0i64;
    loop {
        let page = target
            .scan_records(book, cursor.as_deref(), MAX_PAGE)
            .context("scan target for event ids")?;
        let mut seen = 0usize;
        for record in page {
            seen += 1;
            let Some(rest) = record.entity_id.strip_prefix("marbles_event/") else {
                continue;
            };
            if let Ok(keys) = serde_json::from_str::<Vec<String>>(rest)
                && let Some(seq) = keys.first().and_then(|seq| seq.parse::<i64>().ok())
            {
                max = max.max(seq);
            }
            cursor = Some(record.entity_id);
        }
        if seen < MAX_PAGE {
            break;
        }
    }
    Ok(max)
}

fn write_history(
    source: &Connection,
    target: &mut fungos_records::Store,
    book: &str,
    report: &mut Report,
) -> Result<()> {
    let mut seq = next_event_seq(target, book)?;
    let mut stmt =
        source.prepare("SELECT seq,issue_id,ts,actor,event,detail FROM history ORDER BY seq")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        seq += 1;
        let event = json!({
            "issue_id": row.get::<_, String>(1)?,
            "ts": row.get::<_, i64>(2)?,
            "actor": row.get::<_, String>(3)?,
            "event": row.get::<_, String>(4)?,
            "detail": row.get::<_, String>(5)?,
        });
        target.put_record(
            book,
            &entity("marbles_event", &seq.to_string()),
            "marbles.marbles_event@1",
            1,
            event,
        )?;
        report.events += 1;
    }
    Ok(())
}
