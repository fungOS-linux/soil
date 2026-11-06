# Contacts Spore

Status: **personal contacts + explicit CRM projections, 2026-11-06.**

```text
contacts namespace   personal, authoritative contact records
crm namespace        only explicit first-name + token projections
```

## Actions

```text
list
show <id|token>
add <id> <first> <last> <email> [phone] [title]
set <id|token> <field> <value>
share <account-id> <contact-id|token> [first-name]
unshare <account-id> <contact-id|token>
shared
help
```

## Namespaces

```text
--contacts contacts --crm crm:fpl
--contacts contacts --crm crm:puget-audio
```

The host maps namespaces to books and enforces grants.

## No merge

There is no merge and no bulk share. Personal contacts stay in the contacts
book. `share` writes only:

```text
first name
opaque contact token
account id
```

Email, phone, notes and the rest of the contact body never enter the shared CRM.

## UI

`ui/editor.lua` renders the personal contact rail, contact detail, token, and
explicit unshare actions. Contact editing is this Spore's job; the CRM only
links to contacts by id/token.

## Build

```sh
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/contacts_spore.wasm module.wasm
```
