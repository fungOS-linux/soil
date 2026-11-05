# `soil:records`

Durable, schema-tagged records with a change feed and a host-owned revision
model.

```wit
package soil:records@0.1.0;

key { collection, id }
entry { key, schema, revision, updated-at-ms, origin, deleted, body }
page { entries, next }
```

Host surface:

```text
get
scan
upsert
delete
changes
apply
```

The host owns durability, revisions, tombstones, and sync. A Spore owns the
collection name, schema, body encoding, and domain meaning.

## Contacts host

`crates/soil-records-host` adapts one `fungos-records` book as one
`soil:records` collection:

```rust
let host = ContactsRecordsHost::open(
    "~/.local/share/fungos/records",
    "personal/avery/contacts",
    "contact",
)?;
```

Implemented:

- `get`
- `scan`
- `upsert`
- `delete`
- contact schema validation

Currently `changes` and `apply` return `unsupported`; the native
`records.sync.*` Unibus provider remains the sync path. Moving sync into the
Spore-facing contract is a later step.

The host returns `revision = updated-at-ms = op HLC` and
`origin = local device id` for local writes. Both records are tombstones:
deleted entries disappear from `scan` but remain in the local op log.

## Guest SDK

```rust
use soil_sdk::records::{Book, TypedBook, Record};

let contacts = Book::new("contact");
let page = contacts.scan(None, 50)?;

for entry in page.entries {
    let body: Contact = serde_json::from_slice(&entry.body)?;
}
```

Typed:

```rust
#[derive(serde::Serialize, serde::Deserialize)]
struct Contact { /* ... */ }

impl soil_sdk::records::Record for Contact {
    const COLLECTION: &'static str = "contact";
    const SCHEMA: &'static str = "fungos.contacts/contact";
}

let contacts = TypedBook::<Contact>::open();
let page = contacts.scan(None, 50)?;
```

## Boundary

- `soil:records` is not `soil:appdata`: records are durable, schema-tagged,
  searchable by the domain, and shareable by book policy.
- `soil:records` is not `soil:vfs`: records own metadata/body; large blobs stay
  in VFS/storage.
- The host never lets a Spore choose a book, app id, SQL table, or host path.
