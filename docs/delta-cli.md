# Delta CLI index

Status: **inventory, 2026-11-07.** Contract observed from `delta 0.24.0`; not a
stable API promise. This note records what the CLI owns, what our integrations
use today, and where the GUI is the projection layer.

Delta is the current client and presentation layer. Soil should consume its
JSON/JSONL and share links through an adapter; it should not reimplement Delta
or depend on DeltaDB as the Desktop store.

## Command surface

| Surface | Commands | What it provides | Used today |
|---|---|---|---|
| Account/org | `auth status/login/wait/cancel/logout`, `org list/current/create/switch/invite` | Signed-in account, organizations, team invites | `org list` from `delta-pi`; login is external |
| Thread read | `thread list/show/search/query/tree`, `thread status` | Local/account threads, transcript projections, search, parent/child tree, producer resume state | `thread list`, `thread status` |
| Producer recording | `thread contract/create/append/watch/stop` | Contract versions, create/reuse by `--source` + `--external-id`, schema-1 JSONL append with receipts, read-only watch, turn supersession | `contract`, `create`, `append`, `watch` |
| Thread sharing | `thread share/unshare/invite`, `thread fork`, `thread sync`, `thread discard/delete/archive` | Organization visibility, explicit invites, share IDs/links, fork at a version, local cleanup | `share`, `unshare` |
| Thread collaboration | `thread mount`, `thread merge`, `thread send` | Keep worktrees mounted, merge into parent/child, agent-to-agent message | `thread mount` |
| Worktrees | `worktree create/attach/detach/list/mount/merge` | Standalone timelines, thread attachment, checkout adoption, explicit frontier merge | `create`, `attach`, `list`, `mount --existing-checkout` |
| MCP/integration | `delta mcp search`, `delta integration register` | Account-connected MCP tools and integration advertisement | Not used |
| DB inspection | `delta db users/stats/attribute/worktree-files/node/root` | Local account stores, streaming size stats, node/root blob decoding, worktree bytes | Not used |

Global flags: `--json` for JSON output, `--debug` for hidden commands/options,
and `--dev`/`--trunk`/`--stable`/`--database-dir` to select a data directory.

## Worktree model exposed by the CLI

```text
worktree create <repo> [revision]
  -> standalone worktree at HEAD or a Git revision

worktree attach --thread <thread> <reference>
  -> add it to a thread without mounting

worktree mount <reference> <directory>
  -> synchronize a shared worktree, or --existing-checkout to adopt one

worktree merge --into <destination> <source> [--version <frontier>]
  -> merge a source timeline into another timeline of the same worktree

thread create --parent <thread> [--shared-worktrees|--isolated-worktrees]
  -> child shares the parent's worktrees, or gets private forks that merge back

thread mount --owner <owner> <thread>
  -> keep the thread's worktrees mounted while connected

thread merge --into <destination> <source>
  -> merge worktrees between a parent and a direct subthread
```

A worktree is therefore a standalone, timeline-bearing object, not a copied
project directory. Threads attach worktrees; sharing with child threads is an
explicit choice. The GUI Edits view is a projection of those worktree timelines;
the sync channel is the mounted worktree.

## How `delta-pi` uses it today

`delta-pi` is a producer plugin. It does not touch DeltaDB.

- `thread contract` checks the recording contract version.
- `thread create --source pi --external-id <session>` creates or reuses the
  thread; the source/external-id pair is the stable producer identity.
- `thread status` resumes sequence numbers after a restart.
- `thread append` sends schema-1 JSONL records and reads receipts
  (`recorded`, `duplicate`, `fenced`). Records remain in a local outbox until
  acknowledged.
- `thread watch` delivers teammate activity and turn supersession.
- `worktree create`, `worktree attach`, `worktree list`, and
  `worktree mount --existing-checkout` share the checkout; the mount runs for
  the life of the session.
- `thread share --visibility organization` and `thread unshare` manage audience.

## CLI vs GUI

The CLI is the control/data plane; the GUI is the projection/composition layer.

| CLI | GUI-only or GUI-owned |
|---|---|
| Thread list/show/search/query, transcript JSON/JSONL | Sidebar, tags, pins, transcript rendering |
| Share/unshare/invite, share links | Share dialog, people/team pickers, presence |
| Append/watch receipts, turn state | Comments and reactions composition |
| Worktree create/attach/mount/merge | Edits diff view, file change presentation |
| Org/auth | Notification inbox and deeplinks |
| `delta db` inspection | Local thread collection and settings |

The CLI has no comment-create or reaction command. Comments arrive in
`thread show`/`watch` records, but composing them is GUI-side. Thread tags are
local sidebar state; they are not in the replicated graph. Do not build a bridge
that assumes the CLI can write those.

## Implications for the Soil bridge

- Treat `thread_...`/share links as external `ResourceRef`s, not Soil thread IDs.
- Map worktree references and timelines into Soil worktree/tree records; do not
  copy the Delta store.
- Use `thread watch`/`show` and deeplinks as the activity source; map comments
  and notifications into the global inbox with a registered intent, not a raw
  URL.
- Use `delta db` for parity experiments and storage inspection only; it is not a
  stable integration surface.
- `delta mcp search` is an account tool catalog, not a transport. `mcp.myc`
  remains a host-resolved service name, not an Operator-owned URL/token.
