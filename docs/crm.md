# CRM

Status: **records-backed CRM with brand and project grouping, 2026-11-05.**

The CRM is a records-backed relationship view, not a separate database. It does
not need its own WIT contract: a Spore reads `fungos.crm/*` records through
`soil:records`, `soil:search`, and `soil:graph`.

```text
Company       one of our brands or operating entities
Account       external organization
Contact       person at an account
Assignment    contact belongs to account
Projection    shared subset of a contact
Project       work owned by one of our companies
Activity      note, call, meeting, task, mail-ref
Opportunity   pipeline entry
```

Company sits above Account and Project. One account can work with several of our
brands, so the relationship is a separate record:

```text
Acme Robotics
  account-company: FPL          customer
  account-company: Puget Audio  customer
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

## Company shape

```text
id
name
slug
legal_name
kind              brand | operating | legal
parent_company_id
brands[]
domains[]
status
```

## Account shape

An account is the external organization. It does not carry a single `company_id`;
the per-brand relationship lives in `account-company`.

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

## Account-company shape

```text
account_id
company_id
owner
roles[]           customer | supplier | partner | prospect
status
source
first_interaction
last_interaction
visibility
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

## Project shape

```text
id
company_id        owning brand
name
kind              internal | customer | supplier | build | research | ops
status
owner
summary
start_at
due_at
visibility
```

Project links:

```text
project-account   project_id, account_id, role, primary
project-company   project_id, company_id, role = participant
```

`project.company_id` is the owning brand. `project-company` adds other brands
when a project is joint work. A project can involve several accounts with
different roles.

## Activity shape

```text
kind          call | meeting | task | email | note
name
status
date_start
date_end
duration
description
company_id
project_id
account_id
contact_token
parent_type
parent_id
owner
visibility    private | team
source
source_id
```

## CRM Spore and namespaces

The CRM domain is a Spore, not a host. It reads only namespaced records:

```text
crm        shared CRM: accounts, companies, projects, activities, opportunities
contacts   personal contacts
calls      personal call/activity records
messages   personal conversation threads/messages
```

```text
spores/crm
  accounts
  account <account-id>
  timeline <account-id> [limit]
  pipeline
```

The host maps namespace names to books and enforces read/write per namespace.
The Spore never sees a book path. Contacts, calls and mail stay in personal
books; the shared CRM holds the relationship and references them by stable id
or contact token.

Roles are a host policy layer: the host links only the namespace grants the
current principal's Mycelium roles allow. Unibus/records sync remains
host-mediated transport, not a Spore import.

## Timeline

The account/contact view includes a unified timeline built from CRM activities
and linked conversation messages.

```sh
cargo run -p soil-crm-host -- timeline   --root ~/.local/share/fungos/records   --book personal/avery/crm   --timeline-book personal/avery/chat   --account a-acme
```

Messages are joined through `fungos.graph/link` mention records, so the CRM does
not copy message bodies. The inspector fetches the same projection from
`/api/crm/timeline`.

The timeline is a derived view, not a new authority. See
[Unified timeline](timeline.md) for the Git and audit-log design.

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

The inspector groups by brand first:

```text
FPL
  accounts -> projects -> opportunities -> activity
Puget Audio
  accounts -> projects -> opportunities -> activity
```

The inspector has two views:

```text
team view     brand -> account -> first-name projections -> shared activity
private view  brand -> account -> full contacts -> all activity
```

The mock seed contains five brands, three accounts, four contacts, private
assignments, one explicit first-name projection, five projects, activities, and
opportunities. It is written only by the explicit `seed` command; `serve` never
mutates records.

Acme Robotics is linked to both FPL and Puget Audio. That is the point of
`account-company`: the same external account can have a different relationship
with each brand.

Only Ada is projected to the team by default. Grace, Alan, and Elena are linked
privately and do not appear in the team view.

Accounts imported without an `account-company` link appear under an explicit
`Unassigned` group rather than disappearing.

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

## Link an account to a brand

```sh
cargo run -p soil-crm-host -- assign \
  --account a-acme \
  --company co-fpl \
  --role customer
```

Unassign:

```sh
cargo run -p soil-crm-host -- unassign \
  --account a-acme \
  --company co-puget-audio
```

Linking the same account to another brand creates a second `account-company`
record. It does not duplicate the account, contacts, or activity.

## Import

EspoCRM is the first source:

```sh
cargo run -p soil-crm-espo -- sync --url https://crm.fpl.dev --book personal/avery/crm
```

See [EspoCRM import](crm-espo.md).

## Next

```text
CRM Spore over collection `crm`; no soil:crm WIT
brand -> account -> project -> timeline
graph panel from soil:graph
team book mapping + first-name projections
task and artifact records
call/transcript records
mail refs from unibus-mail / Elmers
```
