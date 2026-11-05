# Marbles tasks

Tasks are **Marbles issues**, not `fungos-records` entries.

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

A future `soil-marbles` host adapter will expose:

```text
list(project, status)
ready(project)
show(id)
create(...)
update(...)
claim(...)
close(...)
```

It will use `mb --json` or the Marbles HTTP API. Endpoint, token, project scope,
retries and transport stay host-owned. A Spore never shells out or sees the
Marbles token.

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
