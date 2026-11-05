# Graph records and search references

Status: **first search slice, 2026-11-05.**

The graph is a derived index over ordinary records. It is not a second
database and it does not grant access to any target.

## Record schemas

A link is one record with schema `fungos.graph/link@1`:

```json
{
  "from": "soil",
  "rel": "references",
  "to": "repository:fungOS-linux/soil",
  "title": "Soil repository",
  "role": "primary"
}
```

- `from` and `to` are locators.
- A bare id such as `soil` means a record in the link's own book.
- A qualified record locator is `record:<book>#<entity>`.
- Other locator schemes are `repository:`, `vfs:`, `url:`, `purl:`, `cpe:`,
  `doi:`, `git:`, and `sha256:`.
- The link record's own `entity_id` is the edge id. Repeated edges are allowed
  when their role, qualifier, or provenance differs.

An external identifier is one record with schema
`fungos.graph/reference@1`:

```json
{
  "node": "soil",
  "scheme": "purl",
  "value": "pkg:cargo/soil@0.1.0",
  "title": "Soil crate"
}
```

## Graph queries

`soil:graph@0.1.0` exposes read-only edge and reference queries over the same
approved books:

```wit
neighbors(node, rel, direction, offset, limit) -> edge-page
references(node, offset, limit)                    -> reference-page
state() / refresh()
```

Examples:

```text
neighbors("record:personal/avery/projects#soil", none, both)
  -> outgoing repository reference
  -> incoming document mention

neighbors("record:personal/avery/projects#soil", "references", out)
references("record:personal/avery/projects#soil")
```

Edges carry `source`, `rel`, `target`, `role`, `title`, and `book`. A link never
grants access; inaccessible targets remain unresolved locators.

## Search integration

`soil:search@0.3.0` adds a bounded `references` list to every hit:

```wit
record reference {
  kind: string,
  id: string,
  rel: option<string>,
  title: option<string>,
}
```

A matching project hit can therefore carry:

```text
repository  references  Soil repository
purl                    Soil crate
```

The search host builds the graph from the same approved books it already
searches. A Spore cannot add a book, locator, or target. References are display
metadata; the caller still fetches the authoritative record, file, or
repository through its owning capability.

Limits:

```text
8,192 graph records per schema per book
16 references per hit
```

The graph index is rebuilt with `search.refresh()` and is never synced.

## Boundaries

- A link does not create or widen access.
- A missing or inaccessible target remains an unresolved locator.
- Graph records are ordinary records: they search, sync, and version normally.
- The graph index is a cache and may be deleted and rebuilt.
- Derived containment, Git structure, and future SBOM components remain
  separate derived sources; they are not written into the op log by search.
