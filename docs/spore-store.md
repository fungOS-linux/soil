# The Spore Store

A store is three things that must not be confused: a **package** that moves, a
**listing** that describes it, and a **service** that ranks and reviews it. This
document fixes the boundary between them, because almost every bad outcome in an
app store (silent updates, editable listings, ratings that nobody earned,
screenshots that do not match the binary) comes from letting one layer speak for
another.

## Layers

| layer | lives in | travels with the package | mutable |
|---|---|---|---|
| package | `spore.toml`, `module.wasm`, `ui/`, declared assets, `docs/man.md`, `spore.lock` | yes, as a tar over IPFS | no; the digest is the identity |
| listing | `[catalog]` and `[presentation].icon` in `spore.toml`, plus a CID for the rich record | the claims, never the bytes | no; a new version is a new listing |
| service | reviews, ratings, review attestations, moderation | never | yes, keyed by digest |

A package is what a Spore needs to run. It is built from the manifest: the
declared module, UI and assets, every `.lua` under `ui/`, `docs/man.md`, and
`spore.toml`. Build inputs (`src/`, `wit/`, `Cargo.*`), tests and repo
documentation are not packaged. Store artwork is a CID, so there is nothing to
exclude: a path is not accepted where a CID is required.

The rule that keeps bundles small is therefore a type rule, not a review rule —
`presentation.icon`, `catalog.record` and every screenshot are `ipfs://`
references. `soil-spore` refuses anything else.

## Manifest surface

```toml
[spore]
summary = "Markdown reader/editor with a hot-reloadable Lua UI"
description = "Read and edit Markdown as one document. The module owns the model; the Lua view owns every pixel."
homepage = "https://github.com/fungOS-linux/spores"

[docs]
usage = ["markdown [FILE]", "markdown --version"]
man = "docs/man.md"

[presentation]
badge = "M"
color = "#8b9dff"
icon = "ipfs://bafy…"

[catalog]
categories = ["productivity"]
keywords = ["markdown", "editor"]
screenshots = [
  { cid = "ipfs://bafy…", caption = "Editing a note with the view hot-reloaded" },
  { cid = "ipfs://bafy…", caption = "The same document rendered on the web surface" },
]
record = "ipfs://bafy…"
```

`summary` is the row; `description` is the paragraph under it. `docs.usage` is
the synopsis `man` prints and `docs.man` is the authored body. Both are in the
package because `man` has to work with no network.

`badge` and `color` stay mandatory and stay local. A monogram is drawn by the
launcher even when nothing resolves; the icon is enrichment for a client that
has already fetched it.

## Categories

One fixed vocabulary, defined in `soil-spore`:

```text
communication   developer-tools   education   games
graphics        media             network     productivity
science         security          system      utilities
```

A free-form category list makes browsing and ranking incomparable across
publishers, and it is how a store ends up with 400 near-duplicate tags. Adding a
category is a one-line change to `CATEGORIES` reviewed like any other schema
change. A listing declares one or more; the store may feature a child of a
category without inventing a new top-level name.

## Reviews and ratings

A rating is a claim about a package, so it must be bound to one:

```text
review = (spore_id, version, package_digest, principal, rating 1..5, body, created_at)
```

The key is not `spore_id` alone: a review of `dev.fungos.markdown 0.1.0` must
not silently apply to `0.2.0`, whose bytes nobody reviewed. The store shows a
**current** aggregate for the newest digest and an **all versions** aggregate
for the app, labelled as such.

Only a principal that can prove an install may review. There is no receipt
ledger: the store issues a single-use nonce, and the host signs an attestation
with the device identity at the moment the review is written:

```text
attestation = (spore_id, version, package_digest, principal, nonce, at)
```

The host will only produce that signature for a package it currently has
installed, and the store verifies the signature, the nonce and the digest. This
is the difference between a rating and a vote: it costs an install, and it costs
one signature when you write something rather than one per install.

Rules, in order of how much they matter:

- **One review per principal per digest.** Editing keeps the previous body in
  history; the aggregate counts the latest.
- **Ratings are public, attestations are not.** The store publishes that a review
  is from a verified installer, never which device wrote it.
- **Ranking uses a prior.** The displayed score is the mean with a prior
  (`(sum + k·m) / (count + k)`, `m` = global mean, `k` ≈ 20), so one five-star
  review does not outrank a hundred four-star ones.
- **Reviews are append-only.** Deleting is hiding: the moderation record keeps
  the reason, the author keeps the right to respond once.
- **A yanked package** keeps its reviews but is not installable and is labelled
  with why.

Ratings never gate install. A capability warning is not a popularity contest.

## Publishing gates, day one

The store has manual review from the first commit. Two gates: a mechanical one,
then a human one.

```sh
soil-spore check <dir> --publish
```

refuses a package that is missing a description, homepage, synopsis, manual,
icon, category, keywords, two screenshots, or a written reason for any grant.
This is the "looks like a listing" gate; it is implemented in `soil-spore` so
the same rules run in a publisher's terminal and in the review queue.

Then a human reads it against a short list. Until the store has a track record,
every publisher is reviewed as new, every release is reviewed, and the default
answer is no:

1. What does it do, in one sentence a person who is not a developer can check
   against the screenshots?
2. Does every grant have a reason that the described feature needs? A network
   grant for an app with no server is a refusal, not a question.
3. Does the package do more than the manifest says — a module acting outside its
   declared capabilities, code fetched at runtime, a view that opens a path?
   Spores are static; a mismatch is a refusal.
4. Are the screenshots of *this* build? A listing whose images cannot be
   reproduced from the package digest is a refusal.
5. Is the licence real and compatible, and is the source available for a free
   licence claim?
6. Payments, wallets, mining, gambling and "install this other thing" flows are
   not accepted in the first period. Not because they are impossible; because
   the review capacity to judge them does not exist yet.

Signing is separate from review. A signed package proves who published it; the
review decides whether it may be listed. `SystemAllowlist` decides whether it
may act as a system surface, and that decision is per id **and** digest **and**
publisher principal.

## The store Spore

The store is itself a Spore, with a system role and the smallest grant set that
can install anything:

```toml
[system]
role = "store"

[grants]
"soil:spore.catalog" = "list installed Spores and read their manuals"
"soil:spore.install" = "fetch, verify and install a signed package"
"soil:consent" = "ask before granting a capability set"
"soil:canvas" = "present the storefront"
"soil:http" = "reach the store origin only"
```

`role = "store"` is not a grant. The host allowlists the exact id, role, digest
and publisher; an unsigned or self-declared store gets nothing.

Installation is a host capability, not a Spore's own power:

```text
listing -> fetch tar by CID -> verify lock -> verify signature ->
verify publisher trust -> diff capabilities -> consent popup ->
write package -> refresh catalog
```

Every step fails closed. A store Spore never holds a key, never writes outside
the install root, and cannot widen a grant at runtime; the consent broker — not
the store — decides what the installed Spore may do. The store is a client of
that broker like anything else.

`soil:spore.install` is deliberately not granted to any other Spore. Side-loading
is a host command with its own confirmation, not an API an app can call.

## Updates

Mycelium updates the host; the store updates Spores. They are separate channels
with separate trust and must never be collapsed into one "update everything"
button.

`store.myc` has an **Updates** view: every installed Spore, the version it runs,
the version each configured source offers, and the reason an update has not been
applied. A Spore is installed *by digest*, so "is it current" is a comparison of
package digests, not version strings.

An update is classified by what would have to change to apply it:

| class | what changed | how it applies |
|---|---|---|
| `live` | only `ui/**/*.lua` | Canvas hot-reloads the view; no restart, no consent |
| `restart` | `module.wasm` or assets | the surface re-instantiates at the next launch |
| `consent` | the grant set grew, or a denial was removed | pending until the user approves the new capability set |
| `system` | id, role, publisher or allowlisted digest changes | pending until reboot **and** explicit approval |
| `hold` | downgrade, revoked publisher, or a source disagreement | never applied automatically |

Most things should be `live` or `restart`, and that is a packaging property, not
an accident: keeping presentation in Lua is what makes a fix shippable without a
restart. The store shows the class in the row, so "updates are waiting" is never
an unexplained badge.

Pending work is a first-class state (`fungos.store.pending@1`) with a reason the
user can read — `needs reboot`, `wants microphone access`, `publisher revoked` —
and an action that applies it. A store never applies `consent`, `system` or
`hold` on its own, and never applies anything while the host is mid-update.

Policy defaults:

```text
live, restart     auto-apply within the rollout window
consent, system   stage and wait
hold              never stage past the notification
```

A source may raise the minimum version ("this release fixes a security issue") but cannot lower a user's policy, cannot skip consent, and cannot force an
update that needs a reboot. The one exception is a revocation: a revoked
publisher's packages stop being installable, and the store says so rather than
quietly uninstalling them.

Rollback keeps exactly one previous digest per Spore. Re-applying it is an
install with its own consent check — there is no privileged downgrade path.

## Multiple stores

Anyone can host a store. The official fungOS store is the default source, not a
special case in the client:

```toml
[[source]]
name = "fungOS"
origin = "https://store.myc"
principal = "mycelium:fungos/release"
index = "ipfs://bafy…"

[[source]]
name = "Home lab"
origin = "https://store.home.myc"
principal = "mycelium:ajm/lab"
index = "ipfs://bafy…"
```

A source is an **append-only signed index**, addressed by CID, with no trusted
service in the request path. The index lists `(seq, add|update|yank, spore_id,
version, package_digest, listing_cid)`; each entry is signed by the publisher's
key. The store operator does not re-sign the catalog: it signs a **delegation**
that authorizes a publisher key for a principal, and the client verifies that
delegation, then the entry, then the package before installing it. A store's
origin can be offline: the index is a CID and can be mirrored like any other
content.

Adding a source is adding an operator key and an origin — the apt `sources.list`
shape, reviewed with a fingerprint on screen. Sources are ordered; when two sources
offer different digests for the same version, the higher-priority source wins and
the row is marked as a disagreement, never merged silently.

What a source does **not** carry:

- Package trust. A publisher's signature travels with the tar; the store
  delegates the publisher key, it does not sign the package.
- Reviews of another store's catalog. Ratings are per source, because they are
  that community's judgement; the *attestations* behind a review are portable, so
  a store may import a review only with its attestation and says when it did.
- Authority to widen grants. Consent stays local.

The official store is the default because it is the one fungOS reviews. Being
default is a configuration, not a privilege: removing it, or adding another
before it, is an ordinary edit to the source list.

### Internal fleet stores

Most fleets will run their own store, and that is the expected shape rather than
a workaround. An internal store is a source like any other whose operator
principal is a fleet role and whose origin is a fleet node:

```toml
[[source]]
name = "Home lab"
origin = "https://store.home.myc"
principal = "mycelium:home/fleet"
index = "ipfs://bafy…"
scope = "fleet"
```

A fleet store carries the things a public store must not: internal Spores that
never leave the network, pinned versions for a certified configuration, and a
review that happens inside the organisation. Mycelium already decides which
roles may read which store, so the same access grant that admits a node to the
fleet admits it to the index; a node outside the fleet cannot resolve
`store.home.myc` at all.

Fleet policy may tighten, never loosen:

- require that system Spores come from the fleet store, by id, digest and
  publisher;
- pin a digest and mark everything else `hold`, which is how a certified
  appliance stays certified;
- stage a rollout in cohorts (`canary`, then the rest) by advertising a newer
  index to a subset of nodes;
- keep reviews internal when the catalog is internal.

A fleet store cannot grant capabilities, cannot bypass consent, and cannot
install past a `hold` set locally. It is the same client with the same rules,
pointed at a different index.

## Index, presenter, and deltas

A source's catalog is an append-only log, not a document that is rewritten on
every change:

```text
entry      = (seq, add|update|yank, spore_id, version, digest, listing_cid, publisher_signature)
delegation = (publisher, publisher_key_fingerprint, publisher_public_key,
              scope, issued_at, expires_at, operator_signature)
checkpoint = (seq, root over entries, optional operator countersignature)
```

The bytes at the source's `index` CID are a checkpoint plus the entries up to it.
A client needs the checkpoint, the source operator's key, and the delegations for
the publishers it is about to install; everything else is optional.

- **The API is a presenter.** `https://store.myc/api/…` exists for search,
  categories, ranking, pagination and any query a client does not want to run
  locally. Every row it returns names the `seq` and digest it came from. When the
  API and the checkpoint disagree, the checkpoint wins: a presenter cannot add a
  package, remove one, or change a digest.
- **A mirror is enough.** A store with no API is just a CID; a CID with no store
  is just a mirror. The official store is a presenter over the same bytes anyone
  else can mirror.
- **Polling costs the same at any size.** The client polls the checkpoint — a few
  hundred bytes, usually served `304` — and only when `seq` advances does it fetch
  `after=<seq>`. Freshness stops depending on how big the catalog is, which is
  what makes auto-update sane: a fleet can poll every minute without pulling a
  10 MB index.

### Deltas, and when CDC is worth it

The entry log *is* the index delta. Packages are a separate question, and
content-defined chunking does not pay for itself yet:

| package change | what still shares bytes | who exploits it |
|---|---|---|
| Lua-only change | every other file | IPFS fixed chunks already share the rest |
| module rebuild | almost nothing: a new wasm shifts every offset | CDC does not help either |
| large assets | mostly-unchanged blobs | CDC helps, once packages are tens of MB |

So: signed snapshots plus the entry log now; measure bytes fetched per update;
add CDC when a package exceeds roughly 10 MB **and** successive versions share
large unshifted regions. If it lands, it is a per-package chunk manifest declared
in `[catalog]`, not a change to the index format.

Signatures follow the same rule: the store operator signs **publisher keys**, not
catalog revisions. A key is delegated once (and rotated or revoked as an explicit
record), so a publisher can append an entry without a store round-trip or a new
operator signature. Entries carry the publisher's signature and name the key that
signed them. The checkpoint stays a Merkle root for truncation and fork detection;
an operator may countersign it periodically, but that is freshness, not the hot
path. Package signatures stay the publisher's and are independent of the
delegation.

A publisher may have several delegated keys — one per device is the normal shape.
Revoking a lost device removes that delegation, not the publisher. A single
non-exportable key cannot be shared across a fleet; hardware-backed signing is a
per-device key that either signs directly or attests the software key.

Revocation is its own record, not an edit to the delegation:

```text
revocation = (publisher, key_fingerprint, revoked_at, reason, operator_signature)
```

### The index on disk

`soil-store` reads this shape, and a publisher writes it:

```json
{
  "schema": 1,
  "source": "fungos",
  "checkpoint": { "seq": 17, "root": "…", "signature": "…" },
  "entries": [
    {
      "seq": 16, "kind": "update",
      "spore_id": "dev.fungos.markdown", "version": "0.2.0",
      "digest": "<spore.lock digest>",
      "package": "ipfs://bafy…", "listing": "ipfs://bafy…",
      "module": "<sha256 of module.wasm, or null>",
      "ui": "<digest over ui/**/*.lua>",
      "grants": ["soil:canvas", "soil:storage"],
      "system": null, "publisher": "mycelium:fungos/release",
      "publisher_key": "<PGP fingerprint>",
      "signature": "…"
    }
  ],
  "delegations": [
    {
      "publisher": "mycelium:fungos/release",
      "fingerprint": "<PGP fingerprint>",
      "public_key": "-----BEGIN PGP PUBLIC KEY BLOCK-----…",
      "scope": "spore",
      "issued_at": 1793833200,
      "expires_at": 0,
      "signature": "…"
    }
  ],
  "revocations": [
    {
      "publisher": "mycelium:fungos/release",
      "fingerprint": "<PGP fingerprint>",
      "revoked_at": 1793919600,
      "reason": "laptop lost",
      "signature": "…"
    }
  ]
}
```

`module` and `ui` are not decoration: they are what makes the update class
computable without fetching two tars. The publisher computes them with the same
functions the client uses (`soil_store::plan::module_digest` and `view_digest`),
so "only the view changed" means the same thing on both ends.

## Propagation

The index and the packages are signed and content-addressed, which is what lets
Mycelium carry them. A node that has an index advertises the checkpoint the way
it advertises a service lease; a peer fetches `after=<seq>` and the tars it does
not have. The bytes can come from any peer, because nothing is trusted on the
strength of who handed it over: entries carry the publisher's signature,
delegations carry the operator's signature, packages carry the publisher's
signature, and all three are verified against the digest the client already
had.

That makes a fleet store a *policy*, not a different transport: point the fleet
at its own index, and the mesh that already moves releases moves the catalog and
the packages too. The official store stays the fallback for a machine that is
alone.

## The store's own bytes

A store is a **CID mirror**, not only a presenter. Three kinds of bytes live on
disk and are served by address:

| path | holds | addressed by |
|---|---|---|
| `--packages DIR` | packed tars, `<digest>.spore` and `<digest>.spore.asc` | package digest |
| `--media DIR` | listings, icons, screenshots, clips | `ipfs://` CID |

`soil-store add --media DIR FILE...` puts a file in the directory and prints its
CID, and `soil-store listing DIR --media DIR` builds the
`fungos.store.listing/1` document for a package tree, stores it, and prints the
listing's CID for `[catalog].record`. A listing is one small JSON document —
summary, prose, categories, keywords, icon, screenshots — and it names the
package **digest** it describes, so a listing can never drift onto another build.

Serving media is not gated the way packages are: a CID *is* the content, so
there is nothing to authorize beyond having the bytes. Packages are gated on the
active catalog because a digest alone says nothing about whether the store still
offers it.

`/api/catalog` is the machine view of the same rows a browser draws: id,
version, digest, package CID, signature path, publisher, grants, listing CID and
the resolved listing document. An agent that can read JSON can browse the store
without a UI, and `GET /package/<digest>` + `GET /signature/<digest>` are the
same two requests an install makes.

`/api/search?q=…&category=…` answers from one implementation, and every word has
to match. That is deliberate: a Spore view, a browser and an agent that rank
differently are three stores, and a shareable link has to reopen the same
result. `/s/<spore-id>` is the shareable form of a package page (`/spore/<id>`
stays as the long spelling), so a link copied out of the store opens the same
listing anywhere the same store is reachable — including a browser projection
of a Spore's own view.

## The planner

`soil-store` is the crate that answers the two questions a storefront has to get
right, and it runs on a reviewer's laptop with no network:

```sh
soil-store index    --spores-root spores/ -o index.json [--source fungos]
soil-store delegate --publisher mycelium:fungos/release --operator-key KEY \
                    (--public-key publisher.asc | --fingerprint KEY) [--days 365] -o delegation.json
soil-store sign     --index index.json --delegation delegation.json \
                    --publisher mycelium:fungos/release --key KEY [--countersign OPERATOR-KEY] --write
soil-store revoke   --index index.json --publisher mycelium:fungos/release --key KEY \
                    --reason "laptop lost" --operator-key OPERATOR-KEY --write
soil-store verify   --index index.json --operator-key operator.asc
soil-store plan     --index index.json --spores-root ~/.local/share/fungos/spores [--json]
soil-store snapshot --index index.json --spores-root ~/.local/share/fungos/spores -o store.snapshot.lua
soil-store serve    --index index.json --spores-root ~/.local/share/fungos/spores [--addr 127.0.0.1:8800]
```

`index` seals a fresh log from a package tree: it numbers the entries, folds them
into the checkpoint root, and addresses each tar by the CID of the bytes it would
hand back. It reads the tree without writing to it, so a stale `spore.lock` is an
error with the tree left as it was — publishing is not the moment to bless a
digest nobody reviewed. Its output is deliberately unsigned: `delegate` and `sign`
are the steps that make it a source.

`delegate` is the operator's one signature per publisher key: it covers the
publisher principal, the key fingerprint, the key itself (by digest), the scope
and the validity window, and it is the only thing a publisher needs from the
store. `sign` binds entries to that key and reseals the checkpoint; `--countersign`
adds the operator's optional checkpoint signature for freshness. `revoke`
withdraws one key without rewriting the delegation that admitted it: the record
is append-only and operator-signed, so a mirror cannot revoke a rival and a lost
device does not revoke the publisher.

Verification order is checkpoint root, then delegations, then entries, then — at
install — the package tar and its PGP sidecar. A revoked key's entries stay in
the log for history but are skipped by `verify` and dropped from `catalog()`: one
lost device must not brick every other package.

`serve` is the loopback presenter: it reads the index and the install root once,
answers `/`, `/updates`, `/spore/<id>`, `/api/plan`, `/api/index`, and — when it
is given `--packages DIR` — the tar (`/package/<digest>`) and its publisher
signature (`/signature/<digest>`) for any digest the active catalog lists. It
never writes to either, and it will not serve a digest the catalog does not
offer, so the directory cannot be used to enumerate the disk. It rescans on
start, so publishing a new index means restarting the service
(`launchctl kickstart -k gui/$UID/dev.fungos.soil-store`).

`plan` classifies every installed Spore against the catalog (`live`, `restart`,
`consent`, `system`, `hold`) and reports current and yanked packages. `snapshot`
emits the table `spores/store/ui/store.lua` renders, so the view never learns how
to read an index and the storefront is testable without a Canvas.

The precedence is `hold` > `system` > `consent` > `live`/`restart`: the most
disruptive truth wins, so an update that both rebuilds the module and grows the
grant set is never advertised as a restart.

## `store.myc`

`store.myc` is a name for a local service, exactly like `ai.myc` and `mcp.myc`:

```text
https://store.myc/           -> the storefront, in a browser
https://store.myc/updates    -> every installed Spore and what it is waiting on
store                        -> the store Spore, in a native Canvas window
store --updates              -> the same list in the terminal
```

Mycelium's loopback gateway owns the name, the certificate and the loopback
binding. Behind it is `soil-store serve` on `127.0.0.1:8800`, installed as
`dev.fungos.soil-store` and routed by `~/.mycelium/gateway/routes.json`:

```json
{ "name": "store", "hosts": ["store.myc"],
  "upstream": "http://127.0.0.1:8800", "service_kind": "store", "priority": 10 }
```

The generated gateway certificate lists `store.myc` in its SANs. If no store
service is running the gateway returns an error rather than forwarding somewhere
else. The store never proxies a third party, and no request leaves the machine to
render a listing.

The catalog still carries identity (`spore_id`, `version`, digest, `grants`,
`system`), not prose, so a card for a package this host has not installed shows
its id and category until listings are fetched by CID.

The host installer is `soil-install`. It verifies a local tar, keeps it at
`.packages/<digest>.spore`, unpacks into a staging directory, and only then swaps
`<root>/<id>`. `.install/<id>.json` is the host's crash-safe state; it records the current and
previous digest. Projecting that state into `fungos.store.installed@1` is the
next step. `rollback` reinstalls the previous digest; `apply` reads the index,
uses the same `soil-store plan`, and applies only `live` and `restart`.
`consent`, `system` and `hold` are reported and never applied. Fetching by CID
and granting the store Spore `soil:spore.install` are still separate steps; the
page remains a report until that capability is wired.

Fetched artifacts (icons, screenshots, the rich record) are content-addressed,
so a browser cache and an IPFS gateway agree about what a listing showed.

## Data model

Stored as `soil:records` schemas so the store is a client of the same storage
as everything else:

```text
fungos.store.entry@1      seq, kind, spore_id, version, digest, listing_cid,
                          publisher, publisher_key, publisher_signature
fungos.store.checkpoint@1 seq, root, countersignature?
fungos.store.delegation@1 publisher, publisher_key_fingerprint, publisher_public_key,
                          scope, issued_at, expires_at, operator_signature
fungos.store.listing@1    spore_id, version, digest, category[], keywords[],
                          icon, screenshots[], record, status, reviewed_by
fungos.store.review@1     spore_id, version, digest, principal, rating, body,
                          created_at, hidden_reason
fungos.store.attest@1     digest, principal, nonce, at, signature
fungos.store.pending@1    digest, class, reason, available_since, policy
fungos.store.source@1     name, origin, principal, operator_key, index, priority, scope
fungos.store.feature@1    category, digest, starts_at, ends_at
fungos.store.installed@1  digest, pinned, policy, previous_digest, source
```

Listings are indexed by digest. `spore_id` and `version` are for display and for
ranges such as `>=0.1 <0.2`; they are never the key.

## Non-goals for this slice

- No payments, subscriptions, refunds, or paid promotion.
- No store-driven update of the host itself. Mycelium owns the binary and the
  unit; a Spore update is a package install with its own policy and consent.
- No server-side ranking beyond the review prior; there is no advertising
  surface to optimise for.
- No publisher accounts beyond Mycelium principals.
- No store-side key generation. The operator key is a Mycelium principal;
  delegations are records it signs, and revocation stays in Mycelium.
