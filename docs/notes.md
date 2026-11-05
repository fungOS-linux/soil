# Notes

Status: **reader + editor + Spore-side backlinks, 2026-11-05.**

```text
dev.fungos.notes Spore
  tree/read/outline/links/search/present/save
  reader rail + Markdown editor + presentation mode
  [[wiki-link]] extraction and backlink scan

soil:vfs / soil:index / soil:vfs-mut
  host-owned workspace, search index and writes
```

There is no notes-specific host or WIT. Notes are workspace files under the
Spore's `notes/` root.

## Editor

The reader uses `ui.editor`, a multiline `text_input` node. The host owns the
edit buffer, selection, IME, dirty state and commit timing. The tree carries the
initial value and an expected version; Save emits the version guard, not a stale
copy of the text.

## Backlinks

The Spore resolves note paths under `notes/`, scans Markdown files in that root,
and parses `[[target]]` / `[[target|label]]` links. Backlinks are returned as
graph-shaped edges with `vfs:<path>` locators:

```json
{
  "source": "vfs:notes/welcome.md",
  "target": "vfs:notes/projects/soil.md",
  "rel": "links",
  "title": "optional label"
}
```

## Present

`notes.present` splits Markdown on `---` and renders a deck. Viewer/Canvas
presentation remains host-owned; the Spore emits the intent.
