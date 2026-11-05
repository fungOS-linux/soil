# Mentions

Mentions are stable locators embedded in text. They are not copied names.

```text
@[Ada Lovelace](record:personal/avery/crm#c-ada)
@[Budget](file:sheets/budget.xlsx)
@[Pilot](record:personal/avery/crm#p-acme-pilot)
@[Fix SSO](marbles://store/fpl/acme/ACME-42)
@[Ada](principal:oidc:https://identity.example#subject-42)
```

## Resolve

`soil:search/mentions.suggest(text, kinds, limit)` returns typed candidates with
the same token form. See [Global search](global-search.md).

## Persist

Tasks are Marbles issues, not Soil records. Their canonical ref is the mention
target. A person is notifiable only when the target is a host-attested
`principal:`. Soil stores only the graph link.

One mention becomes one graph link in the source record's book:

```json
{
  "from": "record:personal/avery/tasks#task-1",
  "rel": "mentions",
  "to": "record:personal/avery/crm#c-ada",
  "title": "Ada Lovelace"
}
```

The link id is deterministic from `book`, source id and target. Re-syncing a body:

- creates new links
- updates the label fallback
- removes links no longer in the body
- never duplicates the same source/target pair

`crates/soil-mentions` provides `parse`, `sync`, `clear`, `links_for_source`, and
`backlinks`.

## Backlinks

Incoming links come from the existing graph:

```text
soil:graph.neighbors(
  node: "record:personal/avery/crm#c-ada",
  rel: "mentions",
  direction: in
)
```

or directly:

```rust
soil_mentions::backlinks(&store, &books, "record:personal/avery/crm#c-ada")?;
```

Graph access is still book-scoped. A link is a locator, never an access grant.

## Limits

```text
256 mentions per source body
one link per source/target pair
10,000 link records scanned per book per sync
```

The token syntax is deliberately simple. Parentheses inside the locator are not
supported yet; paths that need them should be percent-encoded or moved to a
record locator.
