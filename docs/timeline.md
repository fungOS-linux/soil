# Unified timeline

Status: **timeline lives in the CRM Spore; Git and audit adapters are next.**

The timeline is a derived projection. It does not own history.

```text
source records          authority
timeline event          normalized projection
timeline index          rebuildable query surface
```

## Event shape

```text
id
kind
at_ms
at
title
summary
actor
source
source_id
book
channel
thread_id
account_ids[]
company_ids[]
project_ids[]
contact_tokens[]
visibility
```

The first implementation is `spores/crm` over namespaced records:

```text
crm       shared CRM records
contacts  personal contacts
calls     personal call/activity records
messages  personal conversation records
```

The host maps namespaces to books and enforces grants. The Spore never sees a
book path or a cross-book cursor.

## CRM sources

```text
fungos.crm/activity@1
  call | meeting | email | note | task

fungos.conversation/message@1
  linked through fungos.graph/link records
```

A message is attached to CRM entities by mention links:

```text
@[Acme](record:<book>#a-acme)
  -> fungos.graph/link { from: record:<book>#<message>, rel: mentions, to: ... }
```

The timeline joins the message to the account, project, contact token, and brand
without copying message text into the CRM.

## Query

```text
account
company
project
contact token
team/private
before_ms
limit
```

Ordering is newest-first by `(at_ms, id)`. The next cursor uses the same
exclusive `(at_ms, id)` semantics as conversations.

## CLI

```text
timeline <account-id> [limit]
account <account-id>
contact <contact-id>
project <project-id>
```

## Git log is a source, not the timeline

Do not shell out to `git log` in a request path. Import commit metadata:

```text
fungos.git/commit@1
  repo_id
  sha
  parents[]
  author { name, email?, at_ms }
  committer { name, email?, at_ms }
  message
  refs[]
  changed_paths[]
  source_ref
```

Then project:

```text
kind        git.commit
title       short subject
summary     body
actor       author
source_id   repo_id:sha
```

Rules:

- Commit metadata only; file contents stay in the repository/content store.
- Deduplicate by `(repo_id, sha)`.
- Link commits to CRM/projects/Marbles through mention tokens in the commit
  message or an explicit repo/project mapping.
- Never put repository credentials in an event.
- Git author time and commit time are both retained; timeline ordering uses the
  chosen display time explicitly.

Git log is authored history. It is not an audit log.

## Audit log is a different trust boundary

Audit events are append-only:

```text
fungos.audit/event@1
  id
  seq
  origin
  at_ms
  actor
  action
  target
  result
  request_id
  prev_hash
  hash
```

Use a hash chain plus periodic checkpoints. The audit log should not be silently
rewritten by normal records updates. If `soil:records` stores the queryable
projection, the append-only authority should be a separate audit host or
content-addressed segments.

Audit examples:

```text
crm.account.updated
crm.contact.projected
crm.contact.unprojected
records.book.granted
mention.link.created
message.deleted
git.repo.connected
```

Timeline visibility:

```text
team timeline     business events only
private timeline  private CRM + private conversations
audit timeline    explicit audit.read grant
```

Not every audit event belongs in the business timeline. Default to hidden;
project only the ones with a business meaning.

## Why keep them separate

```text
Git log       authored intent
CRM activity  relationship history
Sales         pipeline history
Audit log     system/security truth
```

They can share a view. They must not share authority.

## Next

```text
1. Extract TimelineEvent + adapter trait into crates/soil-timeline.
2. Add a persistent timeline index keyed by (at_ms, id) and entity.
3. Add fungos.git/commit@1 + a native Git collector.
4. Add fungos.audit/event@1 + append-only audit host.
5. Add source filters and "why is this here?" provenance to the CRM inspector.
```
