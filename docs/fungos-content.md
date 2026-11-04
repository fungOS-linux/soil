# fungOS content and index stub

Status: **first slice, 2026-11-03.**

`crates/fungos-content` starts the CID-neutral content layer.

The rule:

> CID is a locator. `ContentId` is identity. The graph owns meaning. The index is derived.

## What exists

- `ContentId` — canonical whole-content BLAKE3 digest, displayed as `blake3:<hex>`.
- `BlobRef` — content id, size, media type, and one or more provider locators.
- `Locator` — `local`, `ipfs`, `iroh` (iroh locator reserved).
- `LocalFsProvider` — content-addressed local filesystem store.
- `IpfsStub` — creates real raw-block CIDv1 locators from SHA-256 and keeps bytes in memory until a Kubo HTTP adapter lands.
- `ContentIndex` — JSON-backed index stub keyed by `ContentId`, with lookup by entity, content id, IPFS CID/Iroh hash, and a lexical search stub.

The IPFS stub emits a **raw-block CIDv1**, not a Kubo UnixFS file CID. That distinction is deliberate: a UnixFS file root is not a whole-file hash. The canonical `ContentId` remains the index key; Kubo’s file CID will be another locator once the HTTP adapter is added.

## Next

1. Mycelium/Iroh transport adapter using the existing authenticated peer identity.
2. Kubo HTTP RPC adapter.
3. SQLite/FTS5 index and translated metadata fields.
4. `fungos:content` host capability for Soil components.
5. Range reads, pin/unpin, provider health, and LOD policy.
