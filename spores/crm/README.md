# CRM Spore

Status: **read-only CRM Spore over namespaced records, 2026-11-06.**

```text
crm        shared CRM: accounts, companies, projects, activities, opportunities
contacts   personal contacts
calls      personal call/activity records
messages   personal conversation threads/messages
```

The host maps namespace names to books and enforces read/write grants. The
Spore never sees a host path. Roles/Mycelium decide which namespace grants are
linked.

## Actions

```text
accounts
account <account-id>
timeline <account-id> [limit]
pipeline
help
```

## Build

```sh
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/crm_spore.wasm module.wasm
```

## Test

`crates/soil-records-host/tests/crm_spore.rs` seeds four books, links the
namespaced host, and runs the real component.
