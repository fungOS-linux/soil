# DeltaDB replication: what it is, and what closing the gap costs

Status: **analysis, 2026-11-07.** Evidence is Delta's public documentation plus a
read-only copy of one account's `data.sqlite`. No binaries were disassembled.
This records what an external transport would have to satisfy before we build
one, so we can decide between adopting Delta's protocol, projecting its state,
or asking the vendor to expose a transport binding.

## Why this exists

Delta's managed worktrees only exist on the machine that made them unless the
account has cross-device sync, which is staff-gated. The value Delta adds is
precisely what lives between commits, so git cannot carry it. We wanted to know
whether the replication gap is closable without reverse-engineering a binary.

Short answer: the state machine is fully present and readable locally, the
payload encoding is not, and the difference decides the build-vs-ask question.

## What the documentation establishes

From [Delta & Git](https://delta.dev/docs/concepts/delta-and-git):

- DeltaDB is the shared database behind a thread. It records messages, comments,
  worktree changes, and file edits as **deltas**.
- A delta records *who* made the change and *where it belongs in the thread's
  history*. Deltas are created continuously; nothing is staged or committed.
- Each connected client holds a copy and Delta keeps the copies in sync.
- Import covers tracked files, uncommitted changes to tracked files, and
  untracked files Git does not ignore. **Untracked ignored files are neither
  recorded nor synced**, so build output and untracked `.env` files stay put.
  Adding a path to `.gitignore` does not protect a file Git already tracks.

From [Delta Worktrees](https://delta.dev/docs/concepts/worktrees):

- Each thread is paired with one or more DeltaDB worktrees that exist *inside*
  DeltaDB; checkouts are the on-disk projection of them.
- Managed checkouts are deliberately **not** git worktrees.
- `.agents/linked` symlinks ignored files to a shared location, but those files
  must be Git-ignored, which also excludes them from DeltaDB tracking.

From [Data Storage](https://delta.dev/docs/privacy-and-security/data-storage),
the server side is entirely Cloudflare:

| Data | Store |
| --- | --- |
| File contents, Git commits, history packs, checkpoints | R2 |
| Thread and Delta worktree deltas used for **active synchronization** | Durable Objects backed by SQLite |
| Thread, repository, collaborator, account metadata | KV and D1 |

So the transport terminates at a Durable Object per history, fed by history
packs. That is a log-shipping protocol, not a Git protocol.

## What one account's database actually contains

Measured on a macOS desktop profile (`schema_version` 71, `commit_generation`
4,184,407, 803 MB after an online backup). Row counts are one snapshot; the
shape is what matters.

### Live state: dense

| Table | Rows | Role |
| --- | --- | --- |
| `histories` | 158 | one row per thread/worktree history |
| `history_timelines` | 158 | timelines within a history |
| `history_actors` | 468 | actor uuid per history |
| `history_versions` | 1,608,437 | version strings, indexed by `version_key` |
| `history_delta_nodes` | 1,608,249 | the delta DAG |
| `history_delta_values` | 2,093,857 | the delta payloads |
| `history_delta_value_packs` | 2,626 | packed payload groups |
| `nodes` | 462,509 | content-addressed blob store |
| `roots` | 210 | hash-keyed root objects |
| `local_history_blobs` | 207 (52 MB) | file contents, inline in SQLite |
| `app_worktrees` / `app_worktree_mounts` | 94 / 107 | worktree identity and local mounts |
| `app_threads` | 28 | thread index |
| `history_connection_states` | 20 | per-actor sync/presence state |

### Replication: queue empty, not never used

Every table on the replication path is empty on this machine:

```text
history_replication_outbox_entries        0
history_replication_outbox_states         0
history_replication_sealed_packs          0
history_replication_archive_packs         0
history_replication_active_timeline_updates 0
history_replica_states                    0
history_timeline_sync_states              0
history_unpushed_delta_ids                0
history_timeline_unpushed_delta_ids       0
history_checkpoint_encoded_objects        0   (and every other history_checkpoint_*)
snapshot_evolution*                       0   (every table)
```

Corroborating flags:

```text
app_worktrees.cloud_worktree_id   0 of 94 populated
local_history_blobs.pending_upload  0 of 207
histories.sync_status             0 for all 158
histories.ever_shared             143 of 158 true
```

Read together: the store is fully populated locally, `ever_shared` is true
almost everywhere, and `sync_status` is uniformly 0.

**An empty outbox does not mean replication never ran.** The outbox is a queue:
entries are drained as packs are sealed and applied, so empty means "nothing
pending right now". The cross-machine comparison below is what settles it, and
it rules out the first reading of these numbers.

### The schema states the protocol

The two tables that would drive replication reveal its shape without any
binary analysis:

```text
history_replication_outbox_entries
    history_key, entry_index, pack_sequence,
    delta_actor, delta_timestamp, encoded_entry_byte_len

history_replica_states
    history_key, generation,
    last_applied_sequence, highest_announced_sequence
```

That is append-only sealed packs, ordered by a monotonic `pack_sequence`,
applied to a peer that tracks `last_applied_sequence` under a `generation`
guard, with `highest_announced_sequence` for the other direction. The DAG even
carries the foreign key: `history_delta_nodes.replication_archive_pack_sequence`.

Versioning is actor/timestamp based with an explicit frontier, matching what
`delta worktree merge --version` calls an "exact source history frontier, not a
Git revision":

```text
history_delta_nodes
    history_key, actor, start_timestamp, end_timestamp,
    parent_version, parent_version_key, application_parent_version
history_timelines
    history_key, timeline_actor, timeline_timestamp,
    version, version_key, fork_metadata, remote_origin,
    boundary_version, origin_is_known, origin_actor, origin_timestamp,
    origin_version, epoch_count, ancestors
```

## Cross-machine comparison: two installations, measured side by side

> Correction. An earlier draft of this section compared two database copies that
> were both taken on the same machine, and read their near-identical counts as
> evidence that data had replicated. Both copies were Studio's. Everything below
> comes from querying the two **live** databases concurrently, one per machine,
> with `system_id` confirming they are distinct installations
> (Studio `37094d20-…`, Neo `e604e2ed-…`).

| Signal | Neo | Studio |
| --- | --- | --- |
| `metadata.schema_version` | 71 | 71 |
| `histories` | 89 | 158 |
| histories with zero delta nodes | — | 39 |
| `app_worktrees` | 64 | 94 |
| `app_threads` | 19 | 29 |
| `app_worktrees.cloud_worktree_id` | 0 of 64 | 0 of 94 |
| `local_history_blobs.pending_upload` | 0 | 0 |

### History identity is a one-way subset

Comparing the full `history_uuid` sets:

```text
shared      89
Studio-only 69
Neo-only     0
```

Neo's set is a strict subset of Studio's. Zero Neo-only histories means Neo never
created a history Studio lacks. Of Studio's 69 extra, 39 carry no delta nodes at
all, so roughly 30 histories with real content exist only on Studio.

### Content diverges in both directions

Per shared history, one row per history:

| Measure | Equal | Studio ahead | Neo ahead |
| --- | --- | --- | --- |
| delta nodes | 62 | 15 histories, 732,906 nodes | 12 histories, 127,709 nodes |
| blobs | 83 | 0 histories | 6 histories, 9,791,451 bytes |

The largest single divergence is history `4471D60B5EF546BDAE6576731AFC80F1`:
267,145 delta nodes on Studio against 63,463 on Neo.

### What the numbers mean

**Neither machine is complete.** Neo is missing roughly 733k delta nodes across
15 shared histories plus about 30 histories entirely. Studio is missing roughly
128k delta nodes and **9.8 MB of file content** that exists only on Neo.

**Blob flow is one-directional.** Neo holds every blob Studio has for the shared
histories, while Studio holds none of Neo's extra content. That is a pull-only
pattern, consistent with fetching content when a thread is opened rather than
replicating continuously.

**Neo is a real author, not a mirror.** Twelve histories where Neo is ahead means
work done on Neo is stranded there. This is the concrete instance of the problem
this document exists to solve.

**No worktree has ever been exported.** `cloud_worktree_id` is null on every
worktree on both installations and no blob has ever been queued for upload.
Whatever moves history identity and blob backfill does not touch the worktree
export path.

So the gap is not "nothing replicates". It is: **identity and blob backfill arrive
lazily in one direction, work authored away from the primary machine does not come
back, and no worktree is ever exported.**

## Verified mapping: CLI reference to database row

`delta worktree list` prints references shaped
`<worktree-uuid>#t=<uuid>.<integer>`. That is not opaque:

```text
<worktree-uuid>  = app_worktrees.history_id  (16-byte BLOB, renders as a uuid)
<t=...> prefix   = history_actors.actor      (TEXT uuid)
<t=...> suffix   = history_timelines.timeline_timestamp
```

Observed: the reference `260693ae-33cf-407b-a3a1-324ae22cdf72` appears as
`app_worktrees.history_id`, and actor `50989ebc-ecab-40eb-9982-5cf7e770c68f`
appears both in `history_actors.actor` and in CLI references as
`#t=50989ebc-ecab-40eb-9982-5cf7e770c68f.550704`.

So a Delta worktree's identity is a history uuid plus an (actor, timestamp)
timeline pointer. That is the handle an external projection needs, and it is
obtainable with SQL alone.

## What documents and database cannot answer

These are the real gaps. They are listed with what would actually settle each,
because only some need a disassembler.

1. **Sealed-pack wire format.** The envelope is visible; the payload encoding
   and framing are not. Only Delta's code produces `sealed_packs`. Needs the
   binary.
2. **Transport endpoint and auth.** Which Durable Object URL, which R2 signed
   URLs, and whether any of it is per-account configurable. Needs the binary or
   vendor documentation.
3. **Worktree label namespace.** The uuid our tooling is handed for a thread
   worktree appears in neither machine's `histories.history_uuid` nor in
   `app_worktrees`. Two candidate explanations were tested and both failed:
   `app_worktree_mounts.namespace` is not the on-disk checkout directory name
   (42 distinct namespaces, 20 checkout directories, zero overlap), and the
   managed-repository uuid used in a checkout's `local` remote does not appear
   in `app_repositories.id`. The label is therefore produced by a layer we have
   not located — most likely encoded into the `state`/`worktree_id` BLOBs, which
   are not raw values (they decode as prefix-shared fragments rather than
   uuids). Needs the binary, or a broader correlation sweep.
4. **What `pending_upload` gates** — blob upload only, or the whole replication
   path.
5. **`nodes.id` semantics.** It is an `INTEGER` primary key with a `refcount`,
   while `roots.id` is a `BLOB`. If `nodes.id` is a truncated content hash we
   could compute node ids externally; if it is a rowid, we cannot. Needs the
   binary.
6. **`history_versions.version` encoding.** Needed to construct a valid
   `parent_version`. Almost certainly an encoded version vector. Needs the
   binary.

## Consequence for the build-versus-ask decision

Split the goal in two, because they have different costs:

**Projection (read DeltaDB, feed our own agents).** Needs items 3 and 5 only,
plus SQL. Item 3 is settled by a second database copy. This is cheap, read-only,
and compatible with any Delta version that does not change the schema — and
`metadata.schema_version` gives us a version to gate on. Mush's `run op` overlay
is already the right shape for this: materialize a VFS workspace, run an
Operator session, apply the diff back.

**Transport (make machines actually converge).** Needs items 1, 2, and 6, all of
which require the binary, and all of which depend on a schema that has already
advanced 71 times. Writing packs into a single-writer 803 MB WAL database that
the running app owns, against a private encoding, is the risky path, and it does
not even finish the job without the peer endpoint.

The recommendation that falls out: **project now, ask rather than reverse-engineer
for transport.** The finding below is a much stronger upstream ask than "please
enable sync", because it names the mechanism.

## The ask

> DeltaDB already ships the replication state machine: an outbox of sealed packs
> keyed by `pack_sequence`, per-replica `last_applied_sequence` under a
> `generation` guard, and a `replication_archive_pack_sequence` back-reference on
> the delta DAG. Those tables are empty precisely when nothing is queued, which
> is a drained queue rather than a missing capability. Please expose a transport
> binding — an endpoint or a callback that drains the existing outbox to a store
> we control — instead of only a staff-gated client feature.

Two supporting notes for that ask:

- Anchor on a **content-addressed node store**, not Git. DeltaDB already has
  `nodes`, `roots`, and root hash keys, so an attachment, a worktree snapshot,
  and a backup object are the same replication primitive, and projections
  (filesystem, object storage, a personal mesh) are interchangeable backends.
- Keep the **privacy boundary Delta already chose**: ignored untracked files do
  not replicate. Any transport we supply should preserve that rule rather than
  quietly widen it.

## Same-user transport from capabilities we already have

The obvious question is whether this can be closed without Delta's cooperation,
using our own stack. The surfaces exist:

| Surface | What it provides |
| --- | --- |
| `soil:vfs@1.0.0` | **the CID authority**: `entry.content-id` is `blake3:<hex>`, "canonical whole-content identity … not a provider CID", with `hydration` = `present` \| `placeholder`, a namespace version, and bounded paging |
| `soil:records` | durable schema-tagged records, change feed, peer sync, separate read/write/sync grants |
| `crates/soil-sync-host` | one authenticated `unibus/peer/1` Iroh session with a per-session router |
| `mush run op` | materialize a VFS workspace, run an Operator session, apply the diff back |

`soil:vfs` already states the model we want. A namespace entry may be a
**placeholder** carrying only a ContentId, and `read` "hydrates a placeholder
through the host's granted content transport." A Spore never supplies a provider,
endpoint, token, lease, or a CID as authority. That is "everything is a CID
symlink" expressed as an interface, with the provider deliberately abstracted.

So the composition is: **sync the namespace entries** (path → `content-id`,
namespace version, hydration state) as the manifest, **replicate the content
objects** those ids name through the host's content transport, and **rebuild
`soil:index` / `soil:search` locally** on the receiving side. `soil:records`
carries the durable structured state around it — thread and message metadata —
not the file manifest, and `soil:index` is explicitly derived rather than
authority, so it must never appear in a sync payload.

Two boundaries decide whether this is a backup tool or a sync system:

1. **It does not sync DeltaDB.** This replicates a projection of the files. Delta's
   `nodes.id` is an `INTEGER` while a `soil:vfs` content id is a blake3 digest, so
   our ids cannot address Delta's nodes without settling open item 5.
2. **Two Delta instances both author deltas.** Files written into a checkout are
   ingested by that machine's Delta as local deltas. Without server-side actor
   reconciliation the two histories fork silently. Safe when one machine writes
   at a time; unsafe for concurrent editing of the same worktree.

An earlier draft of this section named `crates/fungos-content` as the content
substrate. That was wrong and the distinction matters. `fungos-content` is a
small helper crate (blake3 `ContentId`, `BlobRef`, `LocalFsProvider`,
`IpfsStub`) whose only dependent in this workspace is
`crates/soil-conversation`; `soil-vfs-host` keeps its own index with a
`content_id TEXT NOT NULL` column. The interface is `soil:vfs`. Nothing should
be built against `fungos-content` as though it were the store.

## Next experiments, cheapest first

1. Break the divergence down by thread and repository *name* rather than uuid,
   to see which projects are stranded on Neo before deciding what to migrate.
2. Check whether any account setting or environment variable names a sync
   endpoint, to answer item 2 without disassembly.
3. Only then disassemble, scoped to the pack seal/parse path.
