# Contacts

Status: **personal contacts + explicit CRM projections, 2026-11-06.**

The Contacts Spore owns personal contact editing. CRM does not edit contacts.

```text
spores/contacts
  list
  view [selected]
  show <id|token>
  add <id> <first> <last> <email> [phone] [title]
  set <id|token> <field> <value>
  link <account-id> <contact-id|token> [role]
  share <account-id> <contact-id|token> [first-name] [last-name]
  unshare <account-id> <contact-id|token>
  shared
```

Namespaces:

```text
--contacts contacts --crm crm:fpl
```

The host maps them to books and enforces grants.

## Privacy

```text
personal contact     authoritative, stays in contacts namespace
account link         private, stays in contacts namespace
shared projection    first/last name + opaque token only, in CRM
```

There is no merge and no bulk share. Email, phone, notes and the rest of the
contact body never enter the shared CRM by default.

## UI

`spores/contacts/ui/editor.lua` renders the personal contact rail, detail,
token and explicit unshare actions from the records-backed `view` action. There
is no separate contact snapshot store.

Contacts use the standard `fungos.contacts/contact` vCard-shaped record schema.
The contact token is the record `uid`.
