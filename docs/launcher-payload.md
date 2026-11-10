# What a launcher receives

Status: **contract, 2026-11-09.** The launcher's input is `soil:search.page`
verbatim plus the two facts a page cannot carry. There is no launcher payload
type, and there must not be one: a second shape is a second ABI, and the whole
point is that anyone can write a launcher against the same contract.

## The payload

```text
page     soil:search.page  { hits[], next?, generation }
query    string            what the person has typed
selected string?           the hit id the host last reported as focused
```

`page` comes from `soil:search.query`. The host builds this per keystroke and
hands it to the package; the package never calls the capability itself. That is
what keeps a launcher pure projection — no authority, no latency budget of its
own, and it works in a browser where `soil:search` does not exist.

## What a launcher may draw from a hit

| Field | Use | Never |
|---|---|---|
| `kind` | group heading, leading glyph | rendered as text |
| `id` | the intent's payload | shown |
| `title` | primary line, body size | truncated without an ellipsis |
| `subtitle` | secondary line, muted | used for identity |
| `snippet` | optional third line, clipped | trusted as markup |
| `score` | order only | drawn |
| `source` | optional provenance chip | assumed stable |
| `updated-at-ms` | relative time | assumed monotonic |
| `references` | the contextual action panel | treated as access |
| `icon` | `Content::Image { asset }` | assumed present |

`icon` is `asset:sha256:…`, a locator: the host resolves it, and a missing icon
is a named placeholder — the manifest's `[presentation] badge + color` — not a
hole in the row. Same rule as every other surface in this system.

## The two lines that keep it honest

1. **Ranking is the index's; presentation is the package's.** If a launcher could
   override ranking it would need its own copy of the index, and the duplication
   comes back from the other side.
2. **A new field is a change to `soil:search`, not to this payload.** If a
   launcher wants something the page cannot express, the ABI grows for everyone
   or the launcher does without.

## Consequences for the host

`crates/soil-search-host` is an implementation of `soil:search`, not a launcher:
sources → hits → page. Anything that draws — the `launcher.html` page, any
per-launcher JSON — is UI living in the index and should move to a Spore, with
the page served as `application/json` on one route.

Two things to fix there, in order:

- it bindgens `crates/soil-search-host/wit/search.wit`, a **second copy** of the
  contract, instead of the canonical `wit/soil-search.wit`;
- its UI (`launcher.html`, `launcher.rs`) sits inside the index.
