# Global search

`soil:search@0.1.0` is a host-composed discovery surface over the sources a
host has already granted:

```text
files       -> soil:index / soil:vfs
records     -> soil:records books
repositories -> explicit local Git roots
actions     -> reserved for a host action registry
```

It is not authority. A hit is a locator plus display metadata. The caller still
fetches the file through `soil:vfs` or the entry through `soil:records`.

## Surface

```wit
search(query)   -> page
state()         -> search-state
refresh()       -> search-state
```

```wit
query {
  text,
  kinds,       // file / record-entry / repository / action; empty means all supported
  mode,        // substring / fuzzy / full-text / semantic
  offset,
  limit,
}
```

```wit
hit {
  kind,
  id,          // path, book/entity, or action id
  title,
  subtitle,
  snippet,
  score,
  source,
  updated-at-ms,
}
```

## Host behavior

`crates/soil-search-host` currently composes:

- `VfsHost` search over the derived SQLite/FTS5 index
- `fungos-records` substring search over an explicit allowlist of books
- bounded local Git discovery under host-granted roots

Results are merged by score, then title, and paginated. `refresh` rebuilds the
VFS index and advances the search generation.

The host supplies the filesystem root, approved books, and Git roots. A Spore does
not choose a path, book, provider, or source. Semantic file search is enabled
only when the host attaches an embedding provider:

```rust
let search = SearchService::open(vfs_root, records_root, books)?
    .with_embeddings(Arc::new(Model2VecEmbedder::potion_base_8m()?))?;
``` Requesting `action` without an
action source returns no hits rather than inventing one.

## Ranking

- VFS hits retain their derived-index score.
- Exact path promotion happens inside `soil:index`.
- Record hits are newest-first and receive a stable host-side score.
- Semantic mode is available only when the VFS host has an embedding provider;
  otherwise the search returns `unsupported`.

## Boundaries

- Search never grants access. It can only search sources the host opened.
- Search is local and never syncs an index.
- Record text is read from the materialized `fungos-records` view.
- Git search reads repository name, relative id, branch, and a credential-
  stripped origin URL. It never runs Git or reads working-tree bytes.
- App/action results require a future explicit action source; no ambient
  process scan or filesystem crawl.
