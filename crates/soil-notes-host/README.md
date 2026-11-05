# soil-notes-host

Host-side notes policy for the `dev.fungos.notes` Spore.

The Spore never names a host path. This crate owns:

```text
notes root selection and path normalization
escape rejection (absolute paths, ..)
Markdown [[wiki-link]] extraction
fungos.graph/link projection for backlinks
incoming-link queries by note path
```

## Root mapping

```rust
let root = NotesRoot::new("notes")?;
root.resolve("welcome.md")?; // "notes/welcome.md"
root.resolve("../secrets.md")?; // refused
```

## Graph

Each note link becomes:

```json
{
  "from": "vfs:notes/welcome.md",
  "rel": "links",
  "to": "vfs:notes/projects/soil.md",
  "title": "optional label"
}
```

The existing `soil:graph` host indexes those records, so the Notes Spore can
query backlinks by `vfs:<path>` without a second graph database.

## Test

```sh
cargo test -p soil-notes-host
```
