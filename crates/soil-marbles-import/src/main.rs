//! Import the local Marbles SQLite store into a fungos-records book.
//!
//! The source is opened read-only. The target is one book/collection named by
//! the caller; entity ids are `<type>/<json-key>` so the wasm `mb` SoilEngine
//! can read the same book.

use std::{path::Path, process::ExitCode};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, params};
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

fn run() -> Result<()> {
    let mut from = None;
    let mut root = None;
    let mut book = None;
    let mut dry_run = false;
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
        println!("dry-run: no records written");
        return Ok(());
    }
    let root_path = Path::new(&root);
    let mut target = fungos_records::Store::open(root_path, &root_path.join("device_id"))
        .context("open fungos-records")?;
    let mut written = 0usize;
    write_projects(&source, &mut target, &book, &mut written)?;
    write_issues(&source, &mut target, &book, &mut written)?;
    write_deps(&source, &mut target, &book, &mut written)?;
    write_history(&source, &mut target, &book, &mut written)?;
    println!("wrote {written} records into book {book}");
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
    written: &mut usize,
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
        *written += 1;
    }
    Ok(())
}

fn write_issues(
    source: &Connection,
    target: &mut fungos_records::Store,
    book: &str,
    written: &mut usize,
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
            "updated_at": row.get::<_, i64>(14)?,
            "closed_at": row.get::<_, Option<i64>>(15)?,
            "evidence": evidence,
            "metadata": metadata,
            "external_ref": row.get::<_, Option<String>>(18)?,
        });
        target.put_record(book, &entity("issue", &id), "marbles.issue@1", 1, body)?;
        *written += 1;
    }
    Ok(())
}

fn write_deps(
    source: &Connection,
    target: &mut fungos_records::Store,
    book: &str,
    written: &mut usize,
) -> Result<()> {
    let mut stmt =
        source.prepare("SELECT issue_id,depends_on FROM dep ORDER BY issue_id,depends_on")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let child: String = row.get(0)?;
        let parent: String = row.get(1)?;
        let key = format!("{child}\u{1f}{parent}");
        target.put_record(
            book,
            &entity("dependency", &key),
            "marbles.dependency@1",
            1,
            json!({"child": child, "parent": parent}),
        )?;
        *written += 1;
    }
    Ok(())
}

fn write_history(
    source: &Connection,
    target: &mut fungos_records::Store,
    book: &str,
    written: &mut usize,
) -> Result<()> {
    let mut stmt =
        source.prepare("SELECT seq,issue_id,ts,actor,event,detail FROM history ORDER BY seq")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let _source_seq: i64 = row.get(0)?;
        let _issue: String = row.get(1)?;
        let seq = *written as i64 + 1;
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
        *written += 1;
    }
    let _ = params![]; // keep params import available for future outbox import
    Ok(())
}
