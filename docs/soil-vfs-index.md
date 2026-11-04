# `soil:vfs` and `soil:index`

Status: **contract slice, 2026-11-04.**

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
5. `index.state()` to detect a stale/rebuilt generation.

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

## Host mapping

The existing implementation seams are:

- `fungos-vfs`: SQLite namespace, content-addressed blobs, placeholders,
  versions, leases and eviction.
- `fungos-vfs-unibus`: ranged `unibus.content.fetch.v1` hydration.
- `fungos-vfs-fuse`: native macOS/Linux projection.
- `crates/fungos-content`: CID-neutral `ContentId`, locators and derived index
  stub.

A host adapter should map `list/stat/read` onto `fungos-vfs`, and
`browse/search/status` onto a rebuildable SQLite/FTS5 index keyed by
`ContentId`. It must not expose a provider CID as `content-id`.
