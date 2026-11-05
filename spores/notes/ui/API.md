# Notes UI API

## Entrypoint

`ui/editor.lua` exports `M.render(ctx) -> UI tree`.

The view is a pure projection. It has no WIT imports, no filesystem, no network
and no editor authority.

## Snapshot

```lua
{
  root: string?,
  selected: string?,
  notes: { Note }?,
  document: Document?,
  backlinks: { Backlink }?,
  search: { query: string?, hits: { Hit }? }?,
}

Note = {
  path: string,
  title: string?,
  kind: "file" | "directory"?,
  selected: boolean?,
  updated_at_ms: integer?,
}

Document = {
  path: string?,
  title: string?,
  media_type: string?,
  lines: { Line }?,
}

Line = {
  text: string,
  level: integer?,
}

Backlink = {
  source: string?,
  target: string?,
  rel: string?,
  title: string?,
}

Hit = {
  path: string,
  score: integer?,
  snippet: string?,
}
```

## Actions

| Action | Payload | Emitted by |
| --- | --- | --- |
| `notes.open` | `{ path: string }` | note rail and toolbar |
| `notes.search` | `{}` | toolbar |
| `notes.present` | `{ path: string }` | toolbar |

The UI emits intents. Reading, indexing, presenting and any future writes are
host-side.

## Slots

None in this reader slice.

## Components

None. This is an app Spore.

## Theme/config

Uses the global `fungos.ui` profile and `apps["dev.fungos.notes"]`. No app keys
are required yet.

## Stable ids

`notes-root`, `notes-actions`, `notes-title`, `notes-root`, `notes-rail`,
`notes-document`, `notes-links`, `notes-actions`.

## Authority

None in Lua. The logic module reads the host-approved notes root through
`soil:vfs`, search through `soil:index`, and backlinks through `soil:graph`.
The UI never names a host path or transport.
