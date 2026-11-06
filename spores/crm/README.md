# CRM Spore

Status: **CRM Spore over namespaced records, 2026-11-06.**

```text
crm        shared CRM: accounts, companies, projects, activities, opportunities
contacts   personal contacts
calls      personal call/activity records
messages   personal conversation threads/messages
```

The host maps namespace names to books and enforces read/write grants. The
Spore never sees a host path. Roles/Mycelium decide which namespace grants are
linked.

One Spore can target several CRMs:

```sh
--crm crm
--crm crm:fpl
--crm crm:puget-audio
--crm crm:new-ancients
```

Each namespace is a separate shared CRM book. Personal namespaces stay the same.

## Actions

```text
accounts
account <account-id>
contact <contact-id|contact-token>
project <project-id>
timeline <account-id> [limit]
pipeline
activity add <id> <account-id> <kind> <title> <occurred-at> [summary]
opportunity stage <opportunity-id> <stage>
contact link <account-id> <contact-id> [role]
contact share <account-id> <contact-id> [first-name]
contact unshare <account-id> <contact-id>
contacts shared
call add <id> <account-id> <title> <occurred-at> [summary]
email add <id> <thread-id> <account-id> <subject> <sender> <body> <occurred-at-ms>
help
```

## Contact policy

There is no merge and no bulk share.

```text
personal contact       stays in contacts namespace
account link           explicit account-contact record
shared projection      explicit first name + token only
```

`contact link` does not share anything. `contact share` is required before a
team view sees a first-name projection. Calls and emails written by this Spore
go to the personal `calls` / `messages` namespaces with `visibility = private`.

## UI

`ui/editor.lua` renders either the account detail view or a pipeline board. The
snapshot carries `mode = "pipeline"` and `pipeline.stages[]` with cards.

## Build

```sh
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/crm_spore.wasm module.wasm
```

## Test

`crates/soil-records-host/tests/crm_spore.rs` seeds four books, links the
namespaced host, and runs the real component.
