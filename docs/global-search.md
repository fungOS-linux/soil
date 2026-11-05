# Global search

`soil:search@0.5.0` is a host-composed discovery surface over the sources a
host has already granted:

```text
files        -> soil:index / soil:vfs
records      -> soil:records books
Marbles      -> host-granted issue snapshot
principals   -> host-attested identity set
repositories -> explicit local Git roots
spores       -> explicit local Spore manifest roots
peers        -> explicit `mycelium peers --json` snapshot
services     -> explicit Mycelium/Unibus service catalog snapshot
actions      -> reserved for a host action registry
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
  kinds,       // file / record-entry / repository / spore / peer / service /
              // action / task / principal
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
  references, // bounded graph references attached to this hit
}
```


## Typed mentions

`soil:search/mentions` is the `@` autocomplete resolver. It returns typed
candidates with a stable locator token:

```wit
suggest(text, kinds, limit) -> list<candidate>
```

```wit
candidate {
  kind,        // person / account / company / project / task / opportunity /
              // activity / call / thread / message / document / sheet / slide /
              // file / principal
  token,       // record:<book>#<entity> or file:<path>
  label,
  subtitle,
  schema,      // record schema when the candidate came from records
  format,      // md / pdf / docx / xlsx / pptx / ...
  score,
  visibility,  // team / private / workspace
}
```

Records are mapped by schema:

```text
fungos.crm/account@1       -> account
fungos.crm/company@1       -> company
fungos.crm/contact@1       -> person
fungos.crm/project@1       -> project
fungos.crm/opportunity@1   -> opportunity
fungos.crm/activity@1      -> activity
fungos.crm/call@1          -> call
fungos.mail/thread@1       -> thread
fungos.mail/message@1      -> message
fungos.doc/document        -> document
fungos.doc/document@1      -> document
fungos.docs/document@1     -> document
fungos.note/note@1         -> document
fungos.notes/note@1        -> document
fungos.sheet/sheet@1       -> sheet
fungos.sheets/sheet@1      -> sheet
fungos.slide/slide@1       -> slide
fungos.slides/slide@1      -> slide
```

Marbles issues are not records. The host adapter supplies them as `task`
candidates with a `marbles://<store>/<company>/<project>/<issue>` canonical ref.
Principals are supplied by the host as `principal:<actor>` candidates; they are
not records either.

Unknown schemas are not mentionable. Privacy follows the books the host opened:
a team book yields projections, a private book yields private records.

VFS entries are classified by extension:

```text
document   md markdown mdx txt rst adoc org tex doc docx odt rtf epub html
document   pdf                 (format = pdf)
sheet      xls xlsx xlsm ods csv tsv numbers
slide      ppt pptx pptm odp key
file       everything else
```

All VFS mentions use `visibility = workspace`. `format` carries the extension so
a UI can choose the right icon without parsing the path.

The inspection server can load host-granted sources:

```sh
soil-search-web \
  --marbles-snapshot marbles.json \
  --marbles-store store --marbles-company fpl \
  --principals-snapshot principals.json
```

The autocomplete component is [Mention input](mention-input.md).

Insert `token` into the document/task/record and persist a graph link:

```text
fungos.graph/link@1
  rel: mentions
  source: record:<book>#<document-or-task>
  target: record:<book>#c-ada
```

The label is resolved later. Never copy the display name into the mention.

See [Mentions](mentions.md) for persistence and backlinks, and
[Mention input](mention-input.md) for the themable autocomplete component.

The inspection server exposes the same resolver:

```text
GET /api/suggest?q=ada&kinds=person,account&limit=10
```

## Host behavior

`crates/soil-search-host` currently composes:

- `VfsHost` search over the derived SQLite/FTS5 index
- `fungos-records` substring search over an explicit allowlist of books
- bounded local Git discovery under host-granted roots
- local Spore package manifests under host-granted roots
- a host-granted `mycelium peers --json` snapshot
- a host-granted Mycelium/Unibus service catalog snapshot
- optional Microsoft `tgrep` regex search over an explicit root or VFS mount
- graph links and references from `fungos.graph/link@1` and
  `fungos.graph/reference@1` in the approved records books

Results are merged by score, then title, and paginated. `refresh` rebuilds the
VFS index and advances the search generation.

The host supplies the filesystem root, approved books, Git roots, Spore roots,
and discovery snapshots. A Spore does not choose a path, book, provider, or
source. Discovery is opt-in at the host:

```rust
let search = SearchService::open(vfs_root, records_root, books)?
    .with_spore_roots(vec![spore_root])
    .with_mycelium_snapshot_dir(snapshot_dir)?;
```

`snapshot_dir` contains `peers.json` from `mycelium peers --json` and
`services.json` from `mycelium services --json` or an equivalent service
catalog. Search never talks to the Mycelium daemon, opens a network connection,
or mutates peer/service state. A hit only names a local package id, peer origin,
or service id.

Semantic file search is enabled only when the host attaches an embedding provider:

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
- Graph references are derived from approved books only. A reference is a
  locator, not an access grant; see [graph.md](graph.md).
- Git search reads repository name, relative id, branch, and a credential-
  stripped origin URL. It never runs Git or reads working-tree bytes.
- Spore search reads manifests only, not module bytes or asset contents.
- Peer/service search reads host-supplied snapshots; no daemon RPC, network,
  raw socket, or ambient topology scan.
- App/action results require a future explicit action source; no ambient
  process scan or filesystem crawl.
