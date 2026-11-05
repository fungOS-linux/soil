# Tasks

Tasks are ordinary records with mention-aware bodies.

```text
schema: fungos.task/task@1
book:   host-selected, e.g. personal/avery/tasks
```

## Shape

```text
id
title
body                 // may contain @[label](locator) mentions
status               // todo | doing | blocked | done
owner
due_at
project_id
account_id
opportunity_id
contact_token
source_activity_id
visibility           // private | team
created_at_ms
updated_at_ms
```

## CLI

Create:

```sh
cargo run -p soil-tasks -- create \
  --root ~/.local/share/fungos/records \
  --book personal/avery/tasks \
  --title "Send pilot brief" \
  --body 'Ask @[Ada](record:personal/avery/crm#c-ada) for SSO notes.' \
  --owner Avery \
  --due 2026-11-12 \
  --project p-acme-pilot
```

List:

```sh
cargo run -p soil-tasks -- list \
  --root ~/.local/share/fungos/records \
  --book personal/avery/tasks
```

Filter:

```sh
cargo run -p soil-tasks -- list --status todo --owner Avery
```

Complete:

```sh
cargo run -p soil-tasks -- done --id task-...
```

Show with mentions:

```sh
cargo run -p soil-tasks -- show --id task-...
```

## Mention behavior

Creating a task parses the body and writes `mentions` graph links. Updating the
body reconciles them. See [Mentions](mentions.md).

```text
task body
  -> soil_mentions::sync
  -> fungos.graph/link@1
  -> soil:graph backlinks
```

## Next

```text
task UI over `fungos.task/task@1`
my-tasks widget
due/overdue grouping
assign and notify via soil:principal
task ↔ activity timeline
```
