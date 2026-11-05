# `soil:vfs` and `soil:index`

Status: **contract + persistent host index, 2026-11-04.**

These two capabilities make a fast read-only file browser possible without
giving a Spore a host filesystem path or a raw content provider.

## Split

| Contract | Owns | Does not own |
|---|---|---|
| `soil:vfs@1` | canonical workspace namespace, entry metadata, bounded range reads | host paths, directory handles, providers, leases |
| `soil:index@1` | derived browse order, search, snippets, generation | authority, mutations, content bytes |

The rule remains:

> CID is a locator. `ContentId` is identity. The graph owns meaning. The index is derived. Providers are replaceable.

`content-id` in a VFS entry is `blake3:<hex>`, not an IPFS/Kubo CID. A provider
locator can change without changing the entry or the index key.

## Browser loop

1. `index.browse(parent, order, offset, limit)` for an initial page or sort.
2. `index.search(query)` for text/metadata search.
3. `vfs.stat(path)` for each visible row, or use the entries already returned by
   `vfs.ls`.
4. `vfs.read(path, offset, length)` for preview bytes, thumbnails or a paged
   editor.
5. `index.state()` to detect a stale/rebuilt generation, or `index.refresh()`
   to rebuild explicitly.

`vfs.ls` is path-ordered and is the authoritative child list. `index.browse`
may use a faster derived ordering, but it returns paths only; the Spore must
stat before treating an entry as current.

## Limits

- Host canonicalizes paths. Absolute paths, `..`, and host separators are
  `invalid`; the workspace boundary is not a Spore concern.
- `limit` and `length` are host-bounded. Oversize is `over-budget`, never a
  truncation.
- `read` is read-only. Writes, rename and delete are deliberately absent from
  this slice; mutation belongs to a separate grant.
- A placeholder read may hydrate through the host's content transport. The
  Spore never chooses a provider, endpoint, token, or lease.
- The index is rebuildable and may be empty. `not-ready` is honest absence, not
  an empty result.
- Text search is host-owned; snippets are bounded copies of host text, not a
  second content index.
- The derived index is a cache. It lives beside the VFS metadata, never in the
  `records` store, and can be deleted and rebuilt without user-data loss.

## Host mapping

The existing implementation seams are:

- `fungos-vfs`: SQLite namespace, content-addressed blobs, placeholders,
  versions, leases and eviction.
- `fungos-vfs-unibus`: ranged `unibus.content.fetch.v1` hydration.
- `fungos-vfs-fuse`: native macOS/Linux projection.
- `crates/fungos-content`: CID-neutral `ContentId`, locators and derived index
  stub.

A host adapter maps `list/stat/read` onto `fungos-vfs`, and
`browse/search/status` onto a rebuildable SQLite/FTS5 index keyed by
`ContentId`. It must not expose a provider CID as `content-id`.

## Persistent host index

`crates/soil-vfs-host` implements the first backend:

```text
<vfs-root>/.fungos/soil-index.sqlite3
```

or an explicit path with `VfsHost::open_with_index(root, index_path)`.

Schema:

```sql
entries(path PK, content_id, media_type, size, version, hydration,
        pinned, updated_at_ms, kind, child_count, text)
chunks(chunk_id PK, path, content_id, ordinal, text)
chunks_fts USING fts5(chunk_id UNINDEXED, path UNINDEXED, text)
vectors(chunk_id, model, dimension, vector BLOB, norm)
```

- `entries` is the browse/stat view and the source of authoritative paths.
- `chunks_fts` is the full-text tier; results return paths and bounded snippets.
- `vectors` is an optional derived cache for a future embedding backend.
- A source hash over VFS entry metadata avoids rebuilding on every open.
- A VFS change bumps `generation`; a reopen with the same source reuses the
  persisted generation.
- Full-text search also promotes exact path matches, so Spotlight-style recall
  does not bury a filename behind body text.

Search behavior:

| Mode | Backend |
|---|---|
| `substring` | in-memory entry/text scan |
| `fuzzy` | in-memory path ranking |
| `full-text` | SQLite FTS5 + exact path promotion |
| `semantic` | opt-in host embedding provider + vector search |

Vector helpers store `f32` BLOBs and rank by brute-force cosine. That is the v1
baseline. If the workspace outgrows it, add `sqlite-vec`/HNSW/LanceDB behind the
`IndexBackend` trait rather than changing `soil:index`.

Semantic indexing is opt-in:

```rust
let host = VfsHost::open(root)?
    .with_embeddings(Arc::new(Model2VecEmbedder::potion_base_8m()?))?;
```

The default local model is `minishlab/potion-base-8M` (256 dimensions). Without a
provider, `semantic` remains an explicit `unsupported`. A remote provider is
available behind the `remote` feature for host-owned HTTP, not a Spore import.

The index is local and never synced. Sync durable intent through `records` and
content through VFS/Unibus, then rebuild the index per device.
