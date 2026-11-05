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
  editor: { text: string?, expected_version: integer?, dirty: boolean? }?,
  presentation: { path: string?, title: string?, slides: { Slide }? }?,
  slide_index: integer?,
}

Slide = {
  title: string?,
  lines: { string }?,
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
| `notes.edit` | `{ path: string }` | toolbar |
| `notes.save` | `{ path: string, expected_version: integer?, content: string }` | toolbar |
| `notes.previous` | `{}` | presentation bar |
| `notes.next` | `{}` | presentation bar |
| `notes.exit-present` | `{}` | presentation bar |

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
`notes-document`, `notes-links`, `notes-actions`, `notes-present`,
`present-actions`, `present-slide`.

## Authority

None in Lua. The logic module reads the host-approved notes root through
`soil:vfs`, search through `soil:index`, and backlinks through `soil:graph`.
The UI never names a host path or transport.
