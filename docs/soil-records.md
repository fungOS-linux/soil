# `soil:records`

Durable, schema-tagged records with a change feed and a host-owned revision
model.

```wit
package soil:records@0.1.0;

key { collection, id }
entry { key, schema, revision, updated-at-ms, origin, deleted, body }
version { revision, origin }
cas-result { applied, entry }
page { entries, next }
```

Host surface:

```text
get
scan
upsert
compare-and-swap
delete
changes
apply
```

The host owns durability, revisions, tombstones, and sync. A Spore owns the
collection name, schema, body encoding, and domain meaning.

## Contacts host

`crates/soil-host` adapts one `fungos-records` book as one
`soil:records` collection.

Generic:

```rust
let host = RecordsBookHost::open(
    "~/.local/share/fungos/records",
    "personal/avery/mail",
    "message",
)?;
```

Contacts:

```rust
let host = RecordsBookHost::open_contacts(
    "~/.local/share/fungos/records",
    "personal/avery/contacts",
    "contact",
)?;
```

`open_with_schema` requires one exact schema; `open` accepts any non-empty
schema; `open_contacts` additionally validates the vCard-shaped contact body.

Implemented:

- `get`
- `scan`
- `upsert`
- `compare-and-swap`
- `delete`
- `changes`
- `apply`
- materialized revision/origin LWW

`changes` is a materialized change feed over `(updated-at-ms, id)`, including
tombstones. `apply` writes remote entries through the records op log using the
remote revision as HLC and the remote origin as the device. Equal revisions are
ordered lexicographically by origin.

`compare-and-swap` is the single-writer primitive: `expected = none` creates
only when no live record exists, and `expected = some(version)` requires an
exact `(revision, origin)` match. A mismatch returns `applied = false` with the
current entry, so callers can re-read instead of silently overwriting a winner.
It is the primitive used for Marbles claims.

The host returns `revision = updated-at-ms = op HLC` and
`origin = op device id` for local writes. Deleted entries disappear from
`scan` but remain in `changes` and the local op log.

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

## Generic ingest sink

`soil-records-ingest` is a loopback HTTP sink for native adapters and workflow
engines. It is not a CRM API; it accepts schema-tagged records for an allowed
book.

```sh
cargo run -p soil-host --bin soil-records-ingest -- \
  --root ~/.local/share/fungos/records \
  --book personal/avery/crm \
  --schema-prefix fungos.crm/ \
  --token-env SOIL_RECORDS_TOKEN \
  --listen 127.0.0.1:8791
```

Single record:

```json
POST /api/records
{
  "book": "personal/avery/crm",
  "id": "espo:account:a1",
  "schema": "fungos.crm/account@1",
  "body": { "name": "Acme" }
}
```

Batch:

```json
POST /api/records/batch
{
  "records": [
    { "book": "...", "id": "...", "schema": "...", "body": { } }
  ]
}
```

Delete:

```json
{ "book": "...", "id": "...", "schema": "...", "op": "delete" }
```

The sink enforces:

```text
bearer token
book allowlist
schema prefix allowlist
8 MiB body cap
1,000-record batch cap
JSON object bodies
```

It owns no domain meaning. Elmers and other native adapters decide what records
to project. The same records then sync to devices through the normal records
path.

## Boundary

- `soil:records` is not `soil:appdata`: records are durable, schema-tagged,
  searchable by the domain, and shareable by book policy.
- `soil:records` is not `soil:vfs`: records own metadata/body; large blobs stay
  in VFS/storage.
- The host never lets a Spore choose a book, app id, SQL table, or host path.

## Browser inspector

`soil-records-web` is a small read-only loopback UI over the contact host. It
opens the same SQLite book as `records` and reads through `RecordsBookHost`, so
it exercises the `soil:records` path a Spore uses rather than querying SQLite
directly.

```sh
cargo run -p soil-host --bin soil-records-web -- \
  --book personal/avery/contacts \
  --collection contact \
  --listen 127.0.0.1:8787
```

Then open <http://127.0.0.1:8787/>. It shows the contact list with search,
per-contact detail, address/email/phone/URL labels, the raw vCard, record
revision/origin metadata, and a `.vcf` export.

The same inspector renders calendar books when given the event schema:

```sh
cargo run -p soil-host --bin soil-records-web -- \
  --book personal/avery/calendar \
  --collection event \
  --schema fungos.calendar/event \
  --listen 127.0.0.1:8789
```

It switches to an event list, date chips, event detail, recurrence/attendees,
raw VEVENT preview, and `.ics` export. `--kind calendar` is available when the
schema is not enough to infer the view.

Endpoints:

```text
GET /                  HTML inspector
GET /api/records       JSON entries, optional ?q= search
GET /api/records/{id}  one JSON entry
GET /api/export.vcf    live contact cards as vCard
GET /health            liveness
```

The server is read-only, binds to loopback by default, and is an inspection
tool, not the eventual synced application surface.
