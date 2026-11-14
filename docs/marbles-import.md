# Marbles records import

`soil-marbles-import` copies a Marbles SQLite store into a `fungos-records`
book. The source is opened read-only and is never modified.

```sh
cargo run -p soil-marbles-import -- \
  --from ~/.marbles/marbles.db \
  --root ~/.local/share/fungos/records \
  --book personal/avery/marbles \
  --dry-run
```

Remove `--dry-run` to write.

## Consolidating two machines

Running it twice into one book is how two machines become one store, provided the
second run is neither destructive nor order-dependent:

- `--newest-wins` keeps the revision with the later `updated_at`. Without it the
  last import overwrites the first, and which machine is "right" depends on the
  order you ran them in.
- Dependencies already present are skipped: an edge cannot change, so a re-run
  writes nothing new.
- Event ids continue from the highest one already in the book, so two imports
  cannot share a `seq`.
- `--history` is **off by default**. A fleet store carries hundreds of thousands
  of audit rows and every `mb` invocation scans the event collection to seed its
  counter, so loading the archive makes commands slow. The source SQLite file
  stays the archive.

The command prints what it wrote and what it kept, so a consolidation can be
reviewed rather than trusted:

```text
wrote 930 records into book personal/avery/marbles: projects=140 issues=711 deps=79 events=0
kept the newer target revision for 181 issues (--newest-wins)
```

## Record shape

Imported records use one collection shape understood by the Marbles wasm
`SoilEngine`:

```text
issue/["<id>"]                 marbles.issue@1
project/["<slug>"]             marbles.project@1
dependency/["<child>\u001f<parent>"]  marbles.dependency@1
marbles_event/["<seq>"]        marbles.marbles_event@1
```

The importer preserves project/issue ids, labels, metadata, evidence,
dependencies and (with `--history`) history. It does not write to the source.
`folder`, `project_folder`, `outbound_event` and `remote_dep` are not yet
ported.

`--dry-run` prints source counts and writes nothing; the merge plan needs the
target open, so it cannot be previewed without writing.
