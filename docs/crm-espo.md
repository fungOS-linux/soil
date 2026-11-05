# EspoCRM import

Status: **one-way import + incremental sync, 2026-11-05.**

`soil-crm-espo` imports EspoCRM entities into `fungos-records` as raw CRM
records. It is deliberately a host tool: Espo credentials never enter a Spore.

## Entities

```text
Account       -> fungos.crm/account@1
Contact       -> fungos.crm/contact@1
Lead          -> fungos.crm/lead@1
Opportunity   -> fungos.crm/opportunity@1
Call/Meeting/Task -> fungos.crm/activity@1
```

Each record id is namespaced:

```text
espo:contact:<espo-id>
espo:account:<espo-id>
```

The source id, timestamps, owner, website, domains, email, phone, and parent
ids are preserved in the record body. The importer does not write Espo
credentials or raw request headers.

## Auth

Use an Espo API key:

```sh
export ESPO_API_KEY=...
```

or basic auth:

```sh
export ESPO_USERNAME=...
export ESPO_PASSWORD=...
```

The environment variable names are configurable with `--api-key-env`,
`--username-env`, and `--password-env`.

## Run

Full import:

```sh
cargo run -p soil-crm-espo -- sync \
  --url https://crm.fpl.dev \
  --root ~/.local/share/fungos/records \
  --book personal/avery/crm \
  --full
```

Incremental sync:

```sh
cargo run -p soil-crm-espo -- sync \
  --url https://crm.fpl.dev \
  --book personal/avery/crm
```

State is stored at:

```text
<root>/.soil/espo-sync.json
```

It records the last `modifiedAt` per entity. The next run asks Espo only for
rows newer than that. `--since` overrides the state for one run; `--full`
ignores it.

Dry run:

```sh
cargo run -p soil-crm-espo -- sync --dry-run --full
```

## Behavior

- Pages with `maxSize`, `offset`, `orderBy=modifiedAt`, `order=asc`.
- Uses Espo `where` on `modifiedAt` for incremental sync.
- Skips unchanged records when the local schema and body already match.
- Espo `deleted` rows become records tombstones.
- Imported contacts create private `account-contact` assignments. The importer
  never creates a team projection.
- A failed page aborts before the state file is advanced.
- Records are written to one book; multiple collections can be added later by
  the host mapping.

This is one-way: Espo remains the source until a reviewed write-back path is
built. Graph links, contact projections, and team book mapping are separate
CRM work.
