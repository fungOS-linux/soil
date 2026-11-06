# Marbles tasks

Tasks are **Marbles issues**, not copies in a second task schema.

The native/fleet deployment keeps Marbles authoritative. The wasm `mb` path can
run Marbles directly over `soil:records` with the optional `soil` feature; the
issue, dependency, event and project records are the store, and claims use
`compare-and-swap` so two agents cannot both win. Do not introduce
`fungos.task/task`, `soil-tasks`, a second claim table, or a second ready queue.

```text
mb                           issue tracker and task datastore
mb list|ready|show|create    task query and mutation
mb claim|review|close        claim, review and evidence-backed completion
mb dep|label|folder|sprint   dependencies, grouping and commitments
mb plan|portfolio            DAG execution and portfolio views
```

Soil does not mirror issues into records. That would duplicate status, claims,
dependencies, history, sprints and ready semantics.

## References, not copies

A CRM note, document or activity points at the Marbles canonical ref:

```text
marbles://<store>/<company>/<project>/<issue_id>
```

Example mention:

```text
@[Fix SSO](marbles://store/fpl/acme/ACME-42)
```

The mention graph link stores the ref and a display fallback:

```json
{
  "from": "record:personal/avery/crm#act-1",
  "rel": "mentions",
  "to": "marbles://store/fpl/acme/ACME-42",
  "title": "Fix SSO"
}
```

Marbles remains authoritative for status, assignee, claim, dependencies,
evidence, and history.

## Host adapter

The `soil-marbles` adapter exposes:

```text
list(project, status)
ready(project)
show(id)
create(...)
update(...)
claim(...)
close(...)
```

It uses `mb ... --json`. Endpoint, token file, project scope and actor stay
host-owned. A Spore never shells out or sees the Marbles token.

```rust
let mb = MarblesClient::new("mb")
    .with_project("acme")
    .with_actor(principal.actor);
let ready = mb.ready()?;
```

The host can snapshot issues for search and mention autocomplete:

```rust
let index = MarblesIndex::new("store", "fpl", mb.list()?);
let search = search.with_marbles(index);
```

## Mention source

The mention resolver gets Marbles as a source, not as a record schema:

```text
kind: task
token: marbles://...
label: issue title
subtitle: project · status · priority
visibility: host-granted Marbles project access
```

## What not to build

```text
fungos.task/task@1
soil-tasks
a second claim table
a second ready queue
a second dependency graph
a second issue history
```

Use `mb`.
