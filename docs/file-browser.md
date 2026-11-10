# File browser / finder

Status: **design, 2026-11-04.** Contracts: [`soil:vfs`](soil-vfs-index.md) and
[`soil:index`](soil-vfs-index.md).

The browser is one Spore with one authoritative state machine. The GUI, CLI,
Lua, and agents are clients of that state, not separate automation paths.

Its job is a path search across the workspace filesystem: one query over every
root the host has granted, answered from the index rather than by walking the
tree.

## One state, many clients

```text
                 ┌──────────────────────┐
  Canvas GUI ───▶│   BrowserState        │◀─── CLI / Lua / agent
  (render only)  │ query, sort, cursor,  │
                 │ selection, revision,  │
                 │ active finder, plan   │
                 └──────────┬───────────┘
                            │ transitions
                 ┌──────────▼───────────┐
                 │ host capabilities     │
                 │ vfs, index, archive,  │
                 │ api, notifications    │
                 └──────────────────────┘
```

Every client dispatches the same named transitions:

```text
query.set / mode.set / sort.set / cursor.move / page.load
selection.add / remove / toggle / replace / clear
finder.open / finder.focus / finder.close
plan.preview / plan.approve / plan.cancel / plan.execute
```

A transition carries the expected `revision`. Stale transitions are refused,
not reordered. The GUI renders `BrowserState`; it does not synthesize clicks.
An agent changing the query visibly changes the same finder the human sees.

## Fast by default

The default query should feel like grep/FZF, not like a chat prompt:

1. **Literal substring** over path, extracted text, OCR, and metadata. This is
   the always-on tier.
2. **Fuzzy path** ranking for short, half-remembered names.
3. **Full-text** with snippets where the host has FTS/trigram indexes.
4. **Semantic/embedding** only when explicitly requested or when lexical
   results are weak; it is slower and less predictable.

`soil:index.search` exposes `substring`, `fuzzy`, `full-text`, and `semantic`
modes. The current host answers the first three from one rebuildable SQLite/FTS5
index; `semantic` is an explicit `unsupported` until an embedding backend is
granted. OCR and
extracted text are derived fields, never authority; the file remains the
`ContentId` plus bytes in `soil:vfs`.

The browser keeps a resident page, not a full directory copy. Visible rows are
stat'd; previews/page reads fetch only the ranges needed. Placeholder hydration
happens on preview, behind a host lease.

## Prompt queries compile to plans

A natural-language prompt is not a new authority path. It compiles into a
bounded, inspectable plan:

```text
"photos from my Greece trip"
  -> index.search { text: "greece", mode: semantic, media-type-prefix: "image/" }
  -> optional metadata/EXIF/OCR refinement
  -> selection.replace(paths, provenance: query)

"zip those and send them to grandpa"
  -> plan.archive { selection, format: zip }
  -> plan.send { archive, recipient: grandpa, channel: reviewed-target }
  -> preview + approval + receipt
```

A fast local LLM or RLCD can produce the plan, but it cannot execute it.
Resolution ambiguity is shown as a selection/refinement state, not silently
guessed. Actions are host capabilities with budgets and receipts; the browser
never runs `zip`, opens a mail client, or drives the GUI itself.

## Selection is first class

A selection is a set of stable paths/content ids plus the query and index
generation that produced it. That provenance matters:

- human can see why each item is selected
- an agent can refine or union selections without replacing the human's work
- a plan can be revalidated if the namespace changed
- multiple finders can hold independent selections

Selection operations are explicit and revisioned. A click, CLI command, Lua
call, and agent tool all become the same `selection.*` transition.

## Multiple finders

Each finder is a Spore instance with its own `BrowserState` and optional Canvas
surface. They share the host index and VFS namespace but not cursor, query, or
selection. An agent can open a finder scoped to a query or project, or work in
the human's focused finder. The index remains stateless; `index.state()` gives
readiness/generation so each finder can refresh without callbacks.

This is also the multi-agent story: five finders are five state machines over
one derived index, not five GUI automation sessions.

## Scripting boundary

The scripting surface is the same transition API, not accessibility events:

```lua
browser.query { text = "invoice", mode = "substring" }
browser.select_all { limit = 200 }
browser.plan { action = "archive.zip", then = "send:grandpa" }
```

The CLI is the same calls with JSON envelopes. Canvas is the same state rendered
visually. No synthetic key events, no focus theft, no screen-coordinate scripts.

## Limits

- read-only VFS/index slice: no rename/delete/mutation yet
- plan execution is capability-gated and bounded; no arbitrary process spawn
- semantic search quality depends on a host embedding index; absence is an
  explicit `not-ready`, not an empty answer
- OCR/extracted text and embeddings are derived and rebuildable
- one authoritative state per finder; concurrent editors use revision checks
