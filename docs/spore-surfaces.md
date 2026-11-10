# Spore surfaces

A **surface** is a view a Spore ships. Native rendering is the default and the
one everything gets for free; a bundled web page, or a Canvas view that is not
the package's own `[ui] entry`, is a surface the package has to *declare*.

The declaration exists so a host can answer one question before it opens
anything: **can I present this?** A Spore that needs a browser is not a broken
Spore on a host without one — it is a Spore with a surface that host refuses.
Without the declaration nothing can tell the two apart, because
`[launch] surfaces` only says which hosts may launch the package, not what any
one launch would put on screen.

## Native is the default

A Spore that declares no surface still runs. The host renders its `[module]`
and `[ui]` itself; nothing is loaded from a bundle and nothing is negotiated.
This is the preferred shape: it is the one that works on every host, needs no
browser engine, and inherits the host's theme, input routing and window
management instead of reimplementing them.

A surface is for what native rendering cannot express — a page authored in
HTML, or a second Canvas view with its own entry point.

## Declaring one

```toml
[launch]
default = "workbook"
surfaces = ["canvas", "web"]

[[surfaces]]
name = "workbook"
kind = "web"

[[surfaces]]
name = "preview"
kind = "canvas"
entry = "ui/sheet.lua"
```

| Field | Meaning |
|---|---|
| `name` | What `launch(id, surface)` takes, and what `[launch] default` names once surfaces exist. |
| `kind` | `web` or `canvas`. The ABI's `surface-kind` has exactly these two. |
| `entry` | Entry point relative to the package root. Optional: a `web` surface defaults to `web/index.html`, a canvas one to the package's `[ui] entry`. |
| `requires` | An engine or host capability the surface needs beyond its kind. Optional and free text. |

`entry` is written down only when the artifact is not the conventional one, so
the ordinary case is `kind = "web"` and nothing else — which is what the Spores
that already ship `web/index.html` need to add.

Rules the manifest enforces:

- `[launch] default` must name a declared surface. The error names both what was
  asked for and everything the package does declare.
- An unknown `kind` is a parse error that lists the kinds that would have
  parsed.
- A declared kind must appear in `[launch] surfaces` when that list is present;
  the list is an outer bound on what the host may attempt, never a contradiction.
- A `web` entry ends in `.html`, a canvas entry in `.lua`.
- Declaring nothing changes nothing. Every manifest written before `[[surfaces]]`
  existed parses, validates and launches exactly as it did.

## Launching one

`soil:spore/catalog.launch` takes the surface name and fails with
**`unsupported`** when the host cannot present the surface's kind:

```
launch("dev.fungos.workbook", "workbook")          -> launched
launch("dev.fungos.workbook", "workbook")          -> error.unsupported
                                                      (host has no browser view)
launch("dev.fungos.workbook", "spreadsheet")       -> error.not-found
```

A host never launches something it cannot render. `unsupported` is the honest
answer, and it is the whole reason the kind is typed instead of being a string.

`soil-spore` exposes the same decision as `Manifest::resolve_launch`, so a host
and its CLI cannot disagree about it:

```rust
let target = manifest.resolve_launch("workbook", &[SurfaceKind::Canvas])?;
// LaunchError::Unsupported { kind: Web, .. }, code "unsupported"
```

## Assets are paths, covered by the package

A surface names a path inside the package. It never names a content id.

The package **is** the unit of identity and provenance: `spore.lock` lists every
file the Spore ships and the digest is taken over that list. Declaring a surface
adds its entry's directory to what ships — a page is useless without the files
beside it — so `web/index.html`, `web/app.js` and `web/fonts/ui.woff2` are all
covered by the one package digest.

A per-asset CID would be a second version of the same fact, in a second place,
free to drift from the first. It would also put the package's own provenance in
doubt: if a surface resolved independently of the package, the package digest
would stop describing what a browser actually received. Do not add one.

Store artwork is the exception that proves the rule: a catalogue screenshot is
`ipfs://<cid>` because it is *not* in the package. Anything a surface loads at
runtime lives inside the package.

## Neither view is privileged

A web surface is the **human** view. The same Spore's component export — its
actions, its records, its `soil:` calls — is what an **agent** uses. Neither is
the real interface with the other bolted on: the page is for eyes, the export is
for machines, and both are derived from the same package.

That is why a surface does not get its own capability. Launching one is not a
grant, and a browser view is not a wider grant than a native one: the Spore
already had whatever it had, and the surface only decides where the pixels go.

## The general case, not a special case

This is how a bundled application ships as a Spore:

- **A static site.** Hugo, `fpl-sites`, or any generated tree: `kind = "web"`,
  the built output under `web/`, the package digest as the release identifier.
- **A bundled app with a DOM.** A spreadsheet, a graph explorer, a document
  editor whose UI needs a layout engine the host does not have. The logic stays
  in the module; the DOM lives in `web/`.
- **A second Canvas view.** A sheet, a preview, a print layout — a canvas surface
  with its own `entry` and no new host capability at all.

Nothing above is a special case for one project. The declaration says what the
package needs; the host says whether it has it.

## Status

`browser` is a `soil:surface-import` kind and `surface-kind` is in
`soil:spore@0.1.0`. No host in this repository presents a web surface yet: the
ABI and the validation land first, so that a host which does implement one
cannot silently accept a package it would draw a blank for.
