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

## Mock CRM

Start with deterministic mock data before importing real Espo records:

```sh
cargo run -p soil-crm-host -- seed \
  --root ~/.local/share/fungos/records \
  --book personal/avery/crm

cargo run -p soil-crm-host -- serve \
  --root ~/.local/share/fungos/records \
  --book personal/avery/crm \
  --listen 127.0.0.1:8790
```

Open <http://127.0.0.1:8790/>.

The inspector has two views:

```text
team view     account -> first-name projections -> shared activity
private view  account -> full contacts -> all activity
```

The mock seed contains three accounts, five contacts, private assignments, one
explicit first-name projection, activities, and opportunities. It is written
only by the explicit `seed` command; `serve` never mutates records.

Only Ada is projected to the team by default. Grace, Alan, and Elena are linked
privately and do not appear in the team view.

## Link a contact privately

Linking does not share the contact:

```sh
cargo run -p soil-crm-host -- link \
  --account a-acme \
  --contact c-grace \
  --role "VP Engineering"
```

This writes `fungos.crm/account-contact@1` and creates **no**
`account-contact-projection`. Team view stays unchanged; private view shows the
full contact with `shared: false`.

Unlink:

```sh
cargo run -p soil-crm-host -- unlink \
  --account a-acme \
  --contact c-grace
```

Unlinking removes the private assignment and any projection for that account.

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
