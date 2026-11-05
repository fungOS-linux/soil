# Notes

Status: **reader + editor + host link projection, 2026-11-05.**

```text
dev.fungos.notes Spore
  tree/read/outline/links/search/present/save
  reader rail + Markdown editor + presentation mode

soil:vfs / soil:index / soil:graph / soil:vfs-mut
  host-owned workspace, index, graph and writes

soil-notes-host
  notes-root mapping, path escape rejection
  [[wiki-link]] extraction
  fungos.graph/link projection and backlink queries
```

## Editor

The reader uses `ui.editor`, a multiline `text_input` node. The host owns the
edit buffer, selection, IME, dirty state and commit timing. The tree carries the
initial value and an expected version; Save emits the version guard, not a stale
copy of the text.

## Backlinks

The Spore accepts a note path or an explicit graph locator. A path is mapped to
`vfs:<path>` before querying `soil:graph`. `soil-notes-host` writes one
`fungos.graph/link` record per source/target pair when notes are indexed.

## Present

`notes.present` splits Markdown on `---` and renders a deck. Viewer/Canvas
presentation remains host-owned; the Spore emits the intent.
