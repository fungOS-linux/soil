# Marbles records import

`soil-marbles-import` copies the local Marbles SQLite store into a
`fungos-records` book. The source is opened read-only and is never modified.

```sh
cargo run -p soil-marbles-import -- \
  --from ~/.marbles/marbles.db \
  --root ~/.local/share/fungos/records \
  --book personal/avery/marbles \
  --dry-run
```

Remove `--dry-run` to write.

Imported records use one collection shape understood by the Marbles wasm
`SoilEngine`:

```text
issue/["<id>"]                 marbles.issue@1
project/["<slug>"]             marbles.project@1
dependency/["<child>\u001f<parent>"]  marbles.dependency@1
marbles_event/["<seq>"]        marbles.marbles_event@1
```

The importer preserves project/issue ids, labels, metadata, evidence,
dependencies and history. It does not write to the source and does not switch
Marbles over; the SQLite store remains authoritative until cutover.

`--dry-run` prints source counts and writes nothing.
