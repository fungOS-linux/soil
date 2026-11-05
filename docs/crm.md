# CRM

Status: **initial Espo import, 2026-11-05.**

The CRM is a records-backed relationship view, not a separate database.

```text
Account       company/organization
Contact       person
Assignment    contact belongs to account
Projection    shared subset of a contact
Activity      note, call, meeting, task, mail-ref
Opportunity   pipeline entry
```

## Private by default

```text
private contact
  full name, email, phone, notes
private assignment
  account link, role
team projection
  first name + opaque contact token
```

A team sees an account assignment through the projection. It does not
automatically get the contact's email, phone, mail threads, calls, transcripts,
or recordings.

## Account shape

```text
id
name
domains[]
website
type
industry
phone
description
owner
source
source_id
created_at
modified_at
```

## Contact shape

```text
first_name
last_name
name
email
phone
title
account_id
account_name
description
address
owner
source
source_id
created_at
modified_at
```

## Activity shape

```text
kind          call | meeting | task | email | note
name
status
date_start
date_end
duration
description
parent_type
parent_id
owner
source
source_id
```

## Import

EspoCRM is the first source:

```sh
cargo run -p soil-crm-espo -- sync --url https://crm.fpl.dev --book personal/avery/crm
```

See [EspoCRM import](crm-espo.md).

## Next

```text
CRM Spore over collection `crm`
account -> contact projections -> activity timeline
graph panel from soil:graph
team book mapping + first-name projections
call/transcript records
```
