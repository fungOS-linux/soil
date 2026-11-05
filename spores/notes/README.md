# Notes Spore

Status: **reader + editor slice, 2026-11-05.**

A local-first Markdown notes Spore:

```text
VFS workspace -> soil:vfs
search         -> soil:index
backlinks      -> soil:graph
UI             -> fungos.ui + fungos.action
```

The host owns the notes root, indexing, graph mapping, presentation and any
future writes. The Spore only names workspace-relative paths.

## Module actions

```text
tree [path]
read <path>
outline <path>
links <path>
search <text>
backlinks <node>
present <path>
save <path> <version|new|auto> <content>
```

All output is bounded JSON.

## Reader UI

The reader shows:

- note tree
- selected Markdown document
- backlink rail
- top action bar: Search, Present, Open, Edit, Save
- multiline Markdown editor with a version guard and unsaved marker

The `notes.present` intent opens the presenterm-style slide mode. The module
splits Markdown on `---` and preserves headings per slide.

## Preview

```sh
FUNGOS_SDK=/path/to/fungos-sdk \
FUNGOS_MODULES=/tmp/notes-tend \
/path/to/fungos-sdk/bin/tend ui/editor.lua \
  --snapshot ui/snapshot.example.lua --app dev.fungos.notes --no-reload
```

## Build

```sh
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/notes_spore.wasm module.wasm
```

Package with `soil-spore`.
