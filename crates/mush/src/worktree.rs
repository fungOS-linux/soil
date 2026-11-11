//! Worktree and tree records for Mush.
//!
//! The VFS stays the file store. This module records the collaborative
//! objects above it: a tree is a content-addressed manifest of a VFS subtree,
//! a worktree is a named ref into that tree, and a thread owns worktrees. The
//! records live in `soil:records` so they can move to a synced book later
//! without changing the VFS schema.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use fungos_vfs::{ApplyOutcome, ContentId, Expect, Vfs, pull_over_ssh};
use rand::Rng;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use soil_records_host::{RecordsBookHost, records};

const THREAD_SCHEMA: &str = "fungos.thread/v1";
const TURN_SCHEMA: &str = "fungos.turn/v1";
const WORKTREE_SCHEMA: &str = "fungos.worktree/v1";
const TREE_SCHEMA: &str = "fungos.tree/v1";
const REF_SCHEMA: &str = "fungos.ref/v1";
const CHECKOUT_SCHEMA: &str = "fungos.checkout/v1";
const WORKTREE_STATE_SCHEMA: &str = "fungos.worktree-state/v1";

/// The only collection in the device-local book.
const LOCAL_COLLECTION: &str = "local";
const COMMENT_SCHEMA: &str = "fungos.comment/v1";

const SCHEMAS: &[(&str, &str)] = &[
    ("thread", THREAD_SCHEMA),
    ("turn", TURN_SCHEMA),
    ("worktree", WORKTREE_SCHEMA),
    ("tree", TREE_SCHEMA),
    ("ref", REF_SCHEMA),
    ("checkout", CHECKOUT_SCHEMA),
    ("worktree-state", WORKTREE_STATE_SCHEMA),
    ("comment", COMMENT_SCHEMA),
];

#[derive(Debug, Serialize, Deserialize)]
struct Thread {
    id: String,
    title: String,
    parent: Option<String>,
    created_at_ms: u64,
}

/// Reserved for the agent/turn record; worktree v0 creates threads only.
#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
struct Turn {
    thread: String,
    sequence: u64,
    state: String,
    producer: Option<String>,
    created_at_ms: u64,
}

/// A worktree as every device agrees it is.
///
/// Deliberately no path, no base/head tree and no timestamp of the last local
/// look: those describe one machine's view, and putting them in a synced book
/// means two devices trade lies about each other's filesystem.
#[derive(Debug, Serialize, Deserialize)]
struct Worktree {
    id: String,
    name: String,
    thread: String,
    created_at_ms: u64,
}

/// What *this* device currently has for a worktree.
///
/// Lives in the device-local collection. A synced book must never carry it —
/// the path, and which tree this machine last materialised, are the difference
/// between two checkouts of one worktree.
#[derive(Debug, Serialize, Deserialize)]
struct WorktreeState {
    worktree: String,
    path: String,
    base_tree: String,
    head_tree: String,
    updated_at_ms: u64,
}

/// Where a worktree stands on this device.
#[derive(Debug, Serialize)]
struct WorktreeStatus {
    id: String,
    path: String,
    /// The tree this device last materialised.
    local_head: String,
    /// The tree the synced book points at, which may be newer.
    ref_head: Option<String>,
    /// Uncommitted ops in this store's log.
    pending_ops: usize,
}

/// The worktree record as written before the split, when device state shared
/// the synced collection. Read only by the migration.
#[derive(Debug, Deserialize)]
struct LegacyWorktree {
    id: String,
    name: String,
    thread: String,
    path: Option<String>,
    base_tree: Option<String>,
    head_tree: Option<String>,
    created_at_ms: u64,
    updated_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TreeEntry {
    path: String,
    content_id: String,
    size: u64,
    media_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Tree {
    id: String,
    entries: Vec<TreeEntry>,
    created_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Ref {
    name: String,
    worktree: String,
    tree: String,
    updated_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Checkout {
    id: String,
    worktree: String,
    device: String,
    path: String,
    tree: String,
    created_at_ms: u64,
}

/// Reserved for the resource-anchored comment record.
#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
struct Comment {
    resource: String,
    anchor: String,
    body: String,
    author: String,
    created_at_ms: u64,
}

struct State {
    vfs: Vfs,
    /// The synced book: thread, worktree identity, tree, ref, comment.
    records: RecordsBookHost,
    collection: String,
    /// The device-local book: checkout, mount owner, this machine's path and
    /// base/head for a worktree.
    local: RecordsBookHost,
    local_collection: String,
}

impl State {
    fn open(
        root: &Path,
        state_root: &Path,
        book: &str,
        collection: &str,
        store: Option<&Path>,
    ) -> Result<Self> {
        // Content lives in a store that is shared by every root on this host
        // unless one is named. A caller that names its own (a test, a sandbox, a
        // service with its own data directory) cannot be affected by, or leak
        // into, anybody else's writes.
        let vfs = Vfs::open_with(root, fungos_vfs::NodeRole::Full, store)
            .with_context(|| format!("open VFS {}", root.display()))?;
        let records = RecordsBookHost::open(state_root, book, collection)
            .with_context(|| format!("open records {book}/{collection}"))?;
        // A separate book, not merely a separate collection: the book host
        // hands back whatever it holds stamped with the collection it was asked
        // for, so a second collection inside the same book is not a boundary —
        // it reads the synced records and a sync of the main book would carry
        // the device ones. Device state gets its own book, which is a real
        // boundary: nothing that publishes `book` can see it.
        let local_book = format!("{book}.local");
        let local_collection = LOCAL_COLLECTION.to_string();
        let local = RecordsBookHost::open(state_root, &local_book, &local_collection)
            .with_context(|| format!("open records {local_book}/{local_collection}"))?;
        let mut state = Self {
            vfs,
            records,
            collection: collection.to_string(),
            local,
            local_collection,
        };
        state.migrate_device_state()?;
        Ok(state)
    }

    /// Move device state out of the synced collection for stores written before
    /// the split.
    ///
    /// Idempotent: after a record has been rewritten it carries no path, so a
    /// second open is a no-op. Nothing is deleted from the synced collection
    /// until its local half has been written.
    fn migrate_device_state(&mut self) -> Result<()> {
        let entries = self.records.scan_all(10_000)?;
        let mut pending = Vec::new();
        for entry in entries {
            if entry.deleted || !entry.key.id.starts_with("worktree/") {
                continue;
            }
            let Ok(legacy) = serde_json::from_slice::<LegacyWorktree>(&entry.body) else {
                continue;
            };
            // Only records written before the split carry a path.
            if legacy.path.is_none() {
                continue;
            }
            pending.push(legacy);
        }

        for legacy in pending {
            let head = legacy
                .head_tree
                .clone()
                .or_else(|| legacy.base_tree.clone())
                .unwrap_or_default();
            let path = legacy.path.clone().unwrap_or_default();
            let state = WorktreeState {
                worktree: legacy.id.clone(),
                path,
                base_tree: legacy.base_tree.clone().unwrap_or_else(|| head.clone()),
                head_tree: head,
                updated_at_ms: legacy.updated_at_ms.unwrap_or(legacy.created_at_ms),
            };
            self.put_local(
                worktree_state_record(&legacy.id),
                WORKTREE_STATE_SCHEMA,
                &state,
            )?;
            self.put(
                worktree_record(&legacy.id),
                WORKTREE_SCHEMA,
                &Worktree {
                    id: legacy.id.clone(),
                    name: legacy.name,
                    thread: legacy.thread,
                    created_at_ms: legacy.created_at_ms,
                },
            )?;
        }
        Ok(())
    }

    fn put<T: Serialize>(&mut self, id: String, schema: &str, value: &T) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        let entry = records::Entry {
            key: records::Key {
                collection: self.collection.clone(),
                id: id.clone(),
            },
            schema: schema.to_string(),
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body,
        };
        <RecordsBookHost as records::Host>::upsert(&mut self.records, entry)
            .map_err(|error| anyhow!("write record {id}: {error:?}"))?;
        Ok(())
    }

    /// Write a device-local record. Never the synced book.
    fn put_local<T: Serialize>(&mut self, id: String, schema: &str, value: &T) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        let entry = records::Entry {
            key: records::Key {
                collection: self.local_collection.clone(),
                id: id.clone(),
            },
            schema: schema.to_string(),
            revision: 0,
            updated_at_ms: 0,
            origin: String::new(),
            deleted: false,
            body,
        };
        <RecordsBookHost as records::Host>::upsert(&mut self.local, entry)
            .map_err(|error| anyhow!("write local record {id}: {error:?}"))?;
        Ok(())
    }

    fn get_local<T: DeserializeOwned>(&mut self, id: &str) -> Result<Option<T>> {
        let Some(entry) = self.local.get_entry(id)? else {
            return Ok(None);
        };
        if entry.deleted {
            return Ok(None);
        }
        serde_json::from_slice(&entry.body)
            .with_context(|| format!("decode local record {id}"))
            .map(Some)
    }

    fn get<T: DeserializeOwned>(&mut self, id: &str) -> Result<Option<T>> {
        let Some(entry) = self.records.get_entry(id)? else {
            return Ok(None);
        };
        if entry.deleted {
            return Ok(None);
        }
        serde_json::from_slice(&entry.body)
            .with_context(|| format!("decode record {id}"))
            .map(Some)
    }

    fn scan<T: DeserializeOwned>(&mut self, prefix: &str) -> Result<Vec<T>> {
        let entries = self.records.scan_all(10_000)?;
        let mut values = Vec::new();
        for entry in entries {
            // `scan_all` walks the whole book, so the collection has to be
            // checked here or the local and synced books read each other and
            // the split is cosmetic.
            if entry.deleted
                || entry.key.collection != self.collection
                || !entry.key.id.starts_with(prefix)
            {
                continue;
            }
            values.push(
                serde_json::from_slice(&entry.body)
                    .with_context(|| format!("decode record {}", entry.key.id))?,
            );
        }
        Ok(values)
    }

    fn scan_local<T: DeserializeOwned>(&mut self, prefix: &str) -> Result<Vec<T>> {
        let entries = self.local.scan_all(10_000)?;
        let mut values = Vec::new();
        for entry in entries {
            if entry.deleted
                || entry.key.collection != self.local_collection
                || !entry.key.id.starts_with(prefix)
            {
                continue;
            }
            values.push(
                serde_json::from_slice(&entry.body)
                    .with_context(|| format!("decode local record {}", entry.key.id))?,
            );
        }
        Ok(values)
    }

    fn create_worktree(
        &mut self,
        path: &str,
        thread: &str,
        id: Option<&str>,
        name: Option<&str>,
    ) -> Result<Worktree> {
        validate_thread_id(thread)?;
        let path = normalize_path(path)?;
        let tree = snapshot_tree(&self.vfs, &path)?;
        if tree.entries.is_empty() && !path.is_empty() {
            bail!("no files under {path}");
        }
        let id = match id {
            Some(id) => {
                validate_worktree_id(id)?;
                id.to_string()
            }
            None => random_id("wt"),
        };
        if self.get::<Worktree>(&worktree_record(&id))?.is_some() {
            bail!("worktree {id} already exists");
        }

        let thread_record = thread_record(thread);
        if self.get::<Thread>(&thread_record)?.is_none() {
            self.put(
                thread_record,
                THREAD_SCHEMA,
                &Thread {
                    id: thread.to_string(),
                    title: thread.to_string(),
                    parent: None,
                    created_at_ms: now_ms(),
                },
            )?;
        }

        let name = name.map(str::to_string).unwrap_or_else(|| {
            path.rsplit('/')
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("root")
                .to_string()
        });
        let now = now_ms();
        let worktree = Worktree {
            id: id.clone(),
            name,
            thread: thread.to_string(),
            created_at_ms: now,
        };
        self.put(tree_record(&tree.id), TREE_SCHEMA, &tree)?;
        self.put(worktree_record(&id), WORKTREE_SCHEMA, &worktree)?;
        self.put_local(
            worktree_state_record(&id),
            WORKTREE_STATE_SCHEMA,
            &WorktreeState {
                worktree: id.clone(),
                path,
                base_tree: tree.id.clone(),
                head_tree: tree.id.clone(),
                updated_at_ms: now,
            },
        )?;
        self.set_ref(&id, "head", &tree.id)?;
        Ok(worktree)
    }

    fn snapshot_worktree(&mut self, id: &str, path: Option<&str>) -> Result<Worktree> {
        let worktree = self.load_worktree(id)?;
        let mut state = self.load_worktree_state(&worktree.id)?;
        let path = match path {
            Some(path) => normalize_path(path)?,
            None => state.path.clone(),
        };
        let tree = snapshot_tree(&self.vfs, &path)?;
        self.put(tree_record(&tree.id), TREE_SCHEMA, &tree)?;
        state.path = path;
        state.head_tree = tree.id.clone();
        state.updated_at_ms = now_ms();
        self.put_local(
            worktree_state_record(&worktree.id),
            WORKTREE_STATE_SCHEMA,
            &state,
        )?;
        self.set_ref(&worktree.id, "head", &state.head_tree)?;
        Ok(worktree)
    }

    fn load_worktree(&mut self, id: &str) -> Result<Worktree> {
        let id = id.strip_prefix("worktree/").unwrap_or(id);
        self.get(&worktree_record(id))?
            .with_context(|| format!("worktree {id} not found"))
    }

    fn status(&mut self, id: &str) -> Result<WorktreeStatus> {
        let worktree = self.load_worktree(id)?;
        let state = self.load_worktree_state(&worktree.id)?;
        let ref_head = self
            .get::<Ref>(&ref_record(&worktree.id, "head"))?
            .map(|reference| reference.tree);
        let pending_ops = self.vfs.pending(1000)?.len();
        Ok(WorktreeStatus {
            id: worktree.id,
            path: state.path,
            local_head: state.head_tree,
            ref_head,
            pending_ops,
        })
    }

    fn load_worktree_state(&mut self, id: &str) -> Result<WorktreeState> {
        self.get_local(&worktree_state_record(id))?
            .with_context(|| {
                format!(
                    "worktree {id} has no state on this device — mount or snapshot it here first"
                )
            })
    }

    fn load_tree(&mut self, id: &str) -> Result<Tree> {
        let id = id.strip_prefix("tree/").unwrap_or(id);
        self.get(&tree_record(id))?
            .with_context(|| format!("tree {id} not found"))
    }

    fn set_ref(&mut self, worktree: &str, name: &str, tree: &str) -> Result<()> {
        self.put(
            ref_record(worktree, name),
            REF_SCHEMA,
            &Ref {
                name: name.to_string(),
                worktree: worktree.to_string(),
                tree: tree.to_string(),
                updated_at_ms: now_ms(),
            },
        )
    }

    fn checkout(
        &mut self,
        tree: &Tree,
        target: &str,
        worktree: Option<&Worktree>,
    ) -> Result<usize> {
        let target = normalize_path(target)?;
        let mut count = 0;
        for entry in &tree.entries {
            let content_id = ContentId::parse(&entry.content_id)
                .map_err(|error| anyhow!("invalid content id {}: {error:?}", entry.content_id))?;
            let bytes = self.read_content(&content_id)?;
            let path = if target.is_empty() {
                entry.path.clone()
            } else {
                format!("{target}/{}", entry.path)
            };
            self.vfs
                .put(&path, &bytes, &entry.media_type, Expect::Any)?;
            count += 1;
        }
        if let Some(worktree) = worktree {
            let checkout = Checkout {
                id: random_id("co"),
                worktree: worktree.id.clone(),
                device: device_id(),
                path: target,
                tree: tree.id.clone(),
                created_at_ms: now_ms(),
            };
            self.put_local(checkout_record(&checkout.id), CHECKOUT_SCHEMA, &checkout)?;
        }
        Ok(count)
    }

    /// Read an immutable blob from the VFS content store.
    ///
    /// `fungos-vfs` currently exposes lookup by path, not by content id. The
    /// blob path is the documented `.fungos/objects/<shard>/<hex>` layout, and
    /// the bytes are verified against the content id before use.
    fn read_content(&self, id: &ContentId) -> Result<Vec<u8>> {
        // Ask the VFS where the bytes are; do not work it out from the layout.
        // This used to reconstruct `<root>/.fungos/objects/<xx>/<hex>` by hand,
        // which was correct until content moved to the machine-wide store -- at
        // which point the path named a directory that no longer holds anything,
        // and every checkout failed with "No such file". A consumer that knows
        // the storage layout is a consumer that breaks when storage changes.
        let bytes = self
            .vfs
            .content(id)
            .with_context(|| format!("read content {id}"))?;
        let actual = ContentId::from_bytes(&bytes);
        if &actual != id {
            bail!("content mismatch for {id}: blob hashes to {actual}");
        }
        Ok(bytes)
    }
}

pub(crate) fn run(
    root: &Path,
    command: &str,
    mut args: Vec<String>,
    state_root: PathBuf,
    book: String,
    collection: String,
    store: Option<&Path>,
) -> Result<()> {
    let mut state = State::open(root, &state_root, &book, &collection, store)?;
    match command {
        "worktree" => run_worktree(&mut state, &mut args),
        "tree" => run_tree(&mut state, &mut args),
        other => bail!("unknown command {other:?}"),
    }
}

fn run_worktree(state: &mut State, args: &mut Vec<String>) -> Result<()> {
    let Some(subcommand) = args.first().cloned() else {
        return worktree_help();
    };
    args.remove(0);
    match subcommand.as_str() {
        "create" => {
            let thread = take_option(args, "--thread")
                .ok_or_else(|| anyhow!("usage: mush worktree create <path> --thread <thread>"))?;
            let id = take_option(args, "--id");
            let name = take_option(args, "--name");
            ensure_no_options(args)?;
            let path = args
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("usage: mush worktree create <path> --thread <thread>"))?;
            ensure_arity(args, 1)?;
            let worktree = state.create_worktree(&path, &thread, id.as_deref(), name.as_deref())?;
            let local = state.load_worktree_state(&worktree.id)?;
            println!(
                "{}\tthread={}\thead={}",
                worktree.id, worktree.thread, local.head_tree
            );
            Ok(())
        }
        "list" => {
            let thread = take_option(args, "--thread");
            ensure_no_options(args)?;
            ensure_arity(args, 0)?;
            let mut worktrees = state.scan::<Worktree>("worktree/")?;
            worktrees.sort_by(|a, b| a.id.cmp(&b.id));
            for worktree in worktrees {
                if thread
                    .as_deref()
                    .is_some_and(|thread| thread != worktree.thread)
                {
                    continue;
                }
                // A worktree known to the book but never mounted here shows as
                // such rather than borrowing another device's path.
                let (path, head) =
                    match state.get_local::<WorktreeState>(&worktree_state_record(&worktree.id))? {
                        Some(local) => (local.path, local.head_tree),
                        None => ("-".to_string(), "-".to_string()),
                    };
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    worktree.id, worktree.thread, path, head, worktree.name
                );
            }
            Ok(())
        }
        "show" => {
            ensure_no_options(args)?;
            ensure_arity(args, 1)?;
            let worktree = state.load_worktree(&args[0])?;
            println!("{}", serde_json::to_string_pretty(&worktree)?);
            Ok(())
        }
        "snapshot" => {
            ensure_no_options(args)?;
            if args.is_empty() || args.len() > 2 {
                bail!("usage: mush worktree snapshot <id> [path]");
            }
            let worktree = state.snapshot_worktree(&args[0], args.get(1).map(String::as_str))?;
            let local = state.load_worktree_state(&worktree.id)?;
            println!("{}\thead={}", worktree.id, local.head_tree);
            Ok(())
        }
        "checkout" => {
            ensure_no_options(args)?;
            ensure_arity(args, 2)?;
            let worktree = state.load_worktree(&args[0])?;
            let local = state.load_worktree_state(&worktree.id)?;
            let tree = state.load_tree(&local.head_tree)?;
            let count = state.checkout(&tree, &args[1], Some(&worktree))?;
            println!(
                "checked out {count} files from {} into {}",
                tree.id, args[1]
            );
            Ok(())
        }
        "diff" => {
            ensure_no_options(args)?;
            ensure_arity(args, 2)?;
            let left = state.load_worktree(&args[0])?;
            let right = state.load_worktree(&args[1])?;
            let left_head = state.load_worktree_state(&left.id)?.head_tree;
            let right_head = state.load_worktree_state(&right.id)?.head_tree;
            let left_tree = state.load_tree(&left_head)?;
            let right_tree = state.load_tree(&right_head)?;
            print_diff(&left_tree, &right_tree);
            Ok(())
        }
        "status" => {
            ensure_no_options(args)?;
            ensure_arity(args, 1)?;
            let status = state.status(&args[0])?;
            println!("{}", serde_json::to_string_pretty(&status)?);
            if status.ref_head.as_deref() != Some(status.local_head.as_str()) {
                // Not an error: the book may simply be ahead of this device.
                eprintln!(
                    "note: the book points at {} — this device has {}",
                    status.ref_head.as_deref().unwrap_or("nothing"),
                    status.local_head
                );
            }
            Ok(())
        }
        "pull" => {
            let from = take_option(args, "--from").ok_or_else(|| {
                anyhow!("usage: mush worktree pull <id> --from <host> --remote-root <path>")
            })?;
            let remote_root = take_option(args, "--remote-root").ok_or_else(|| {
                anyhow!("usage: mush worktree pull <id> --from <host> --remote-root <path>")
            })?;
            let remote_bin =
                take_option(args, "--remote-bin").unwrap_or_else(|| "fungos-vfs".to_string());
            ensure_no_options(args)?;
            ensure_arity(args, 1)?;
            // Fail before moving bytes if this device has never mounted the
            // worktree: there would be nothing to reconcile against.
            state.load_worktree_state(&args[0])?;
            // The carrier reports what it moved alongside the outcomes: a
            // streamed snapshot carries bytes and entries the outcome list does
            // not describe, so dropping it would hide the one number that says
            // whether a resume actually resumed.
            let (outcomes, transfer) =
                pull_over_ssh(&mut state.vfs, &from, &remote_root, &remote_bin, 500)?;
            let mut applied = 0;
            let mut duplicate = 0;
            let mut diverged = 0;
            let mut snapshot = false;
            for outcome in &outcomes {
                match outcome {
                    ApplyOutcome::Applied => applied += 1,
                    ApplyOutcome::Duplicate => duplicate += 1,
                    ApplyOutcome::Diverged => diverged += 1,
                    // This store had fallen behind a compaction horizon, so the
                    // peer answered with the live namespace rather than a delta
                    // it could not recognise as short.
                    ApplyOutcome::Snapshot => snapshot = true,
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "applied": applied,
                    "duplicate": duplicate,
                    "diverged": diverged,
                    "snapshot": snapshot,
                    "bytes": transfer.bytes,
                    "content_bytes": transfer.content_bytes,
                    "resumed_from": transfer.resumed_from,
                })
            );
            if diverged > 0 {
                // Fast-forward only in v0: a divergent op is never applied on
                // top of local work, it is surfaced.
                bail!("{diverged} op(s) diverged; this worktree needs reconciling");
            }
            Ok(())
        }
        "schema" => {
            ensure_no_options(args)?;
            ensure_arity(args, 0)?;
            print_schemas()
        }
        "help" | "--help" | "-h" => worktree_help(),
        other => bail!("unknown worktree command {other:?}"),
    }
}

fn run_tree(state: &mut State, args: &mut Vec<String>) -> Result<()> {
    let Some(subcommand) = args.first().cloned() else {
        return tree_help();
    };
    args.remove(0);
    match subcommand.as_str() {
        "snapshot" => {
            ensure_no_options(args)?;
            if args.len() > 1 {
                bail!("usage: mush tree snapshot [path]");
            }
            let path = args.first().cloned().unwrap_or_default();
            let tree = snapshot_tree(&state.vfs, &path)?;
            state.put(tree_record(&tree.id), TREE_SCHEMA, &tree)?;
            println!("{}\t{} files", tree.id, tree.entries.len());
            Ok(())
        }
        "show" => {
            ensure_no_options(args)?;
            ensure_arity(args, 1)?;
            let tree = state.load_tree(&args[0])?;
            println!("{}", serde_json::to_string_pretty(&tree)?);
            Ok(())
        }
        "list" => {
            ensure_no_options(args)?;
            ensure_arity(args, 0)?;
            let mut trees = state.scan::<Tree>("tree/")?;
            trees.sort_by(|a, b| a.id.cmp(&b.id));
            for tree in trees {
                println!(
                    "{}\t{} files\t{}",
                    tree.id,
                    tree.entries.len(),
                    tree.created_at_ms
                );
            }
            Ok(())
        }
        "diff" => {
            ensure_no_options(args)?;
            ensure_arity(args, 2)?;
            let left = state.load_tree(&args[0])?;
            let right = state.load_tree(&args[1])?;
            print_diff(&left, &right);
            Ok(())
        }
        "checkout" => {
            ensure_no_options(args)?;
            ensure_arity(args, 2)?;
            let tree = state.load_tree(&args[0])?;
            let count = state.checkout(&tree, &args[1], None)?;
            println!(
                "checked out {count} files from {} into {}",
                tree.id, args[1]
            );
            Ok(())
        }
        "schema" => {
            ensure_no_options(args)?;
            ensure_arity(args, 0)?;
            print_schemas()
        }
        "help" | "--help" | "-h" => tree_help(),
        other => bail!("unknown tree command {other:?}"),
    }
}

fn snapshot_tree(vfs: &Vfs, path: &str) -> Result<Tree> {
    let root = normalize_path(path)?;
    if !root.is_empty() && vfs.file(&root)?.is_some() {
        bail!("tree snapshot expects a directory, {root} is a file");
    }
    let prefix = if root.is_empty() {
        String::new()
    } else {
        format!("{root}/")
    };
    let mut entries = Vec::new();
    for entry in vfs.list(&root)? {
        let path = if root.is_empty() {
            entry.path.clone()
        } else {
            match entry.path.strip_prefix(&prefix) {
                Some(path) if !path.is_empty() => path.to_string(),
                _ => continue,
            }
        };
        entries.push(TreeEntry {
            path,
            content_id: entry.content_id.to_string(),
            size: entry.size,
            media_type: entry.media_type,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let id = ContentId::from_bytes(&serde_json::to_vec(&entries)?).to_string();
    Ok(Tree {
        id,
        entries,
        created_at_ms: now_ms(),
    })
}

fn print_diff(left: &Tree, right: &Tree) {
    for line in diff_lines(left, right) {
        println!("{line}");
    }
}

fn diff_lines(left: &Tree, right: &Tree) -> Vec<String> {
    let left = left
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.content_id.as_str()))
        .collect::<BTreeMap<_, _>>();
    let right = right
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.content_id.as_str()))
        .collect::<BTreeMap<_, _>>();
    let paths = left
        .keys()
        .chain(right.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut lines = Vec::new();
    for path in paths {
        match (left.get(path), right.get(path)) {
            (None, Some(_)) => lines.push(format!("+ {path}")),
            (Some(_), None) => lines.push(format!("- {path}")),
            (Some(left), Some(right)) if left != right => lines.push(format!("~ {path}")),
            _ => {}
        }
    }
    lines
}

fn print_schemas() -> Result<()> {
    let schemas = SCHEMAS
        .iter()
        .map(|(kind, schema)| (*kind, *schema))
        .collect::<BTreeMap<_, _>>();
    println!("{}", serde_json::to_string_pretty(&schemas)?);
    Ok(())
}

fn worktree_help() -> Result<()> {
    println!(
        "mush worktree create <path> --thread <thread> [--id ID] [--name NAME]\n\
         mush worktree list [--thread <thread>]\n\
         mush worktree show <id>\n\
         mush worktree snapshot <id> [path]\n\
         mush worktree checkout <id> <target>\n\
        mush worktree status <id>\n\
        mush worktree pull <id> --from <host> --remote-root <path>\n\
         mush worktree diff <a> <b>\n\
         mush worktree schema"
    );
    Ok(())
}

fn tree_help() -> Result<()> {
    println!(
        "mush tree snapshot [path]\n\
         mush tree show <id>\n\
         mush tree list\n\
         mush tree diff <a> <b>\n\
         mush tree checkout <id> <target>\n\
         mush tree schema"
    );
    Ok(())
}

fn thread_record(thread: &str) -> String {
    format!("thread/{thread}")
}

fn worktree_record(id: &str) -> String {
    format!("worktree/{id}")
}

/// Device-local companion to `worktree_record`, in the local collection.
fn worktree_state_record(id: &str) -> String {
    format!("worktree-state/{id}")
}

fn tree_record(id: &str) -> String {
    format!("tree/{id}")
}

fn ref_record(worktree: &str, name: &str) -> String {
    format!("ref/{worktree}/{name}")
}

fn checkout_record(id: &str) -> String {
    format!("checkout/{id}")
}

fn normalize_path(path: &str) -> Result<String> {
    if path.contains('\\') {
        bail!("invalid VFS path {path:?}");
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("VFS path cannot contain .."),
            part => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

fn validate_thread_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || id.contains("..")
        || id.contains('\\')
        || id.chars().any(char::is_whitespace)
    {
        bail!("invalid thread id {id:?}");
    }
    Ok(())
}

fn validate_worktree_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("invalid worktree id {id:?}");
    }
    Ok(())
}

fn take_option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == name)?;
    if index + 1 >= args.len() {
        return None;
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Some(value)
}

fn ensure_no_options(args: &[String]) -> Result<()> {
    if let Some(option) = args.iter().find(|arg| arg.starts_with("--")) {
        bail!("unknown option {option}");
    }
    Ok(())
}

fn ensure_arity(args: &[String], expected: usize) -> Result<()> {
    if args.len() != expected {
        bail!("expected {expected} argument(s), got {}", args.len());
    }
    Ok(())
}

fn random_id(prefix: &str) -> String {
    format!("{prefix}_{:016x}", rand::rng().random::<u64>())
}

fn device_id() -> String {
    std::env::var("MUSH_DEVICE")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "local".into())
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

    fn state() -> (tempfile::TempDir, State) {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let state = State::open(
            &dir.path().join("vfs"),
            &dir.path().join("records"),
            "mush-test",
            "worktree",
            Some(&store),
        )
        .unwrap();
        (dir, state)
    }

    /// A store written before the split keeps its worktree, and the device
    /// state it used to carry moves to the local book instead of being dropped.
    #[test]
    fn the_migration_splits_a_pre_split_worktree_record() {
        let dir = tempfile::tempdir().unwrap();
        let (vfs_root, records_root) = (dir.path().join("vfs"), dir.path().join("records"));
        let store = dir.path().join("store");
        {
            let mut state = State::open(
                &vfs_root,
                &records_root,
                "mush-test",
                "worktree",
                Some(&store),
            )
            .unwrap();
            // The shape written before the split: portable identity and device
            // state in one record.
            let legacy = serde_json::json!({
                "id": "wt_old",
                "name": "old",
                "thread": "thread-1",
                "path": "projects/old",
                "base_tree": "tree-base",
                "head_tree": "tree-head",
                "created_at_ms": 5,
                "updated_at_ms": 9,
            });
            state
                .put(worktree_record("wt_old"), WORKTREE_SCHEMA, &legacy)
                .unwrap();
        }

        // Reopening migrates.
        let mut state = State::open(
            &vfs_root,
            &records_root,
            "mush-test",
            "worktree",
            Some(&store),
        )
        .unwrap();
        let local = state.load_worktree_state("wt_old").unwrap();
        assert_eq!(local.path, "projects/old");
        assert_eq!(local.head_tree, "tree-head");
        assert_eq!(local.base_tree, "tree-base");

        let portable = state.load_worktree("wt_old").unwrap();
        assert_eq!(portable.id, "wt_old");
        assert_eq!(portable.thread, "thread-1");
        let body = serde_json::to_string(&portable).unwrap();
        assert!(!body.contains("projects/old"), "{body}");

        // Idempotent: a second open does not duplicate or lose anything.
        let mut again = State::open(
            &vfs_root,
            &records_root,
            "mush-test",
            "worktree",
            Some(&store),
        )
        .unwrap();
        assert_eq!(
            again
                .scan_local::<WorktreeState>("worktree-state/")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn snapshots_are_content_addressed_and_round_trip() {
        let (_dir, mut state) = state();
        state
            .vfs
            .put(
                "projects/demo/README.md",
                b"hello",
                "text/markdown",
                Expect::Any,
            )
            .unwrap();
        state
            .vfs
            .put(
                "projects/demo/src/main.rs",
                b"fn main() {}",
                "text/x-rust",
                Expect::Any,
            )
            .unwrap();

        let tree = snapshot_tree(&state.vfs, "projects/demo").unwrap();
        assert_eq!(tree.entries.len(), 2);
        assert!(tree.id.starts_with("blake3:"));
        assert_eq!(tree.entries[0].path, "README.md");
        assert_eq!(tree.entries[1].path, "src/main.rs");

        state
            .put(tree_record(&tree.id), TREE_SCHEMA, &tree)
            .unwrap();
        let loaded = state.load_tree(&tree.id).unwrap();
        assert_eq!(loaded.id, tree.id);
        assert_eq!(loaded.entries.len(), 2);
    }

    #[test]
    fn worktree_snapshot_diff_and_checkout() {
        let (_dir, mut state) = state();
        state
            .vfs
            .put(
                "projects/demo/README.md",
                b"hello",
                "text/markdown",
                Expect::Any,
            )
            .unwrap();
        state
            .vfs
            .put(
                "projects/demo/src/main.rs",
                b"fn main() {}",
                "text/x-rust",
                Expect::Any,
            )
            .unwrap();

        let worktree = state
            .create_worktree("projects/demo", "thread-1", Some("wt_test"), Some("demo"))
            .unwrap();
        assert_eq!(worktree.thread, "thread-1");
        assert!(state.get::<Thread>("thread/thread-1").unwrap().is_some());
        let base_head = state.load_worktree_state("wt_test").unwrap().head_tree;
        let base = state.load_tree(&base_head).unwrap();

        state
            .vfs
            .put(
                "projects/demo/src/main.rs",
                b"fn main() { changed }",
                "text/x-rust",
                Expect::Any,
            )
            .unwrap();
        state
            .vfs
            .put(
                "projects/demo/src/lib.rs",
                b"pub fn lib() {}",
                "text/x-rust",
                Expect::Any,
            )
            .unwrap();
        let updated = state.snapshot_worktree("wt_test", None).unwrap();
        assert_eq!(updated.id, "wt_test");
        let head_tree = state.load_worktree_state("wt_test").unwrap().head_tree;
        let head = state.load_tree(&head_tree).unwrap();
        let diff = diff_lines(&base, &head);
        assert!(diff.contains(&"~ src/main.rs".to_string()), "{diff:?}");
        assert!(diff.contains(&"+ src/lib.rs".to_string()), "{diff:?}");

        let count = state
            .checkout(&base, "checkouts/demo", Some(&worktree))
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(state.vfs.get("checkouts/demo/README.md").unwrap(), b"hello");
        assert_eq!(
            state.vfs.get("checkouts/demo/src/main.rs").unwrap(),
            b"fn main() {}"
        );
        assert_eq!(state.scan_local::<Checkout>("checkout/").unwrap().len(), 1);
        // The split: the synced book carries identity, this device carries the
        // path and the trees it materialised.
        assert!(state.scan::<Checkout>("checkout/").unwrap().is_empty());
        assert_eq!(
            state
                .scan::<WorktreeState>("worktree-state/")
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            state
                .scan_local::<WorktreeState>("worktree-state/")
                .unwrap()
                .len(),
            1
        );
        let portable = serde_json::to_string(&state.load_worktree("wt_test").unwrap()).unwrap();
        assert!(!portable.contains("path"), "{portable}");
        assert!(!portable.contains("head_tree"), "{portable}");
    }
}
