# Spore packages

A Spore is a manifest plus a Lua UI, and optionally a module (WASM) that owns
semantics the view cannot: a composition such as chrome or the storefront ships
no module, and therefore cannot call a host capability at all. `soil-spore`
builds and verifies the package. It does not publish, host, or resolve
anything.

```text
markdown/
  spore.toml              identity, engine, grants, launch, assets, docs, catalog
  module.wasm             logic
  ui/editor.lua           presentation (Canvas JITs it)
  docs/man.md             the authored manual body
  signatures/<keyid>.asc  detached PGP, excluded from the lock and the tar
  spore.lock              generated, included in the tar
```

Distribution is a **tar over IPFS**. Soil does not own an archive format, a
registry, or a key server.

A package is what the Spore needs to *run*, not everything in its directory:

```text
spore.toml
<module.path>
<ui.entry>  and every other .lua under ui/
<chrome sources>
<asset paths>
<docs.man>
spore.lock
```

Build inputs (`src/`, `wit/`, `Cargo.toml`, `Cargo.lock`), tests, tools, and repo
documentation (`README.md`, `ui/API.md`) are inputs to the build, not payload.
The Lua UI is a directory artifact: how an author splits a view is their
business, so every `.lua` under `ui/` ships and the manifest only names the
entry. Everything under `ui/lib/` therefore travels with the view.

**Store artwork never ships.** `presentation.icon`, `catalog.record` and each
`catalog.screenshots[].cid` must be `ipfs://` CIDv1 references; a local path is
refused at parse time. That keeps screenshots and video out of the bundle by
construction rather than by discipline.

## Manifest

```toml
schema = 1

[spore]
id = "dev.fungos.markdown"   # stable reverse-DNS
name = "markdown"
version = "0.1.0"
license = "MIT"
summary = "Markdown reader/editor with a hot-reloadable Lua UI"
description = "Read and edit Markdown as one document."
homepage = "https://github.com/fungOS-linux/spores"

[engine]
soil = ">=0.1"
target = "wasm32-wasip2"
abi = { canvas = "1.0.0" }

[module]
path = "module.wasm"

[ui]
entry = "ui/editor.lua"
jit = true
hot_reload = true

[launch]
default = "canvas"
surfaces = ["web", "native", "canvas"]
# Optional: the composed Lua Canvas opens, when the launcher starts a view
# rather than the module. Composition writes it into the runtime modules dir.
# canvas_module = "spore-dev-inbox"

[grants]
"soil:canvas" = "present the reader/editor surface"

[assets]
"site-map" = { path = "assets/site-map.png", media_type = "image/png" }

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
  { cid = "ipfs://bafy…", caption = "Editing a note" },
  { cid = "ipfs://bafy…", caption = "The same document on the web surface" },
]
record = "ipfs://bafy…"
```

`summary` is the listing row; `description` is the paragraph under it. `[docs]`
is what `man` prints: `usage` is the synopsis, `man` names the authored
Markdown body, which ships in the package because a manual must work offline.

`[catalog]` is the store surface. `categories` comes from the fixed
`CATEGORIES` vocabulary in `soil-spore`; a free-form namespace makes browsing
and ranking incomparable across publishers. `screenshots` and `record` are
content-addressed references — see `docs/spore-store.md`.

Validation is strict because a package is executable content:

- `id` is a stable reverse-DNS identifier; `version` is semver-ish.
- every path is relative, has no `..`, no backslash, no control characters, and
  no symlink. `module.wasm` must end in `.wasm`; the UI entry in `.lua`.
- a Spore may omit `[module]`. What remains is a view: `ui.entry` is required,
  and without a module there is nothing that can call a grant.
- `launch.surfaces` is a subset of `web`, `native`, `canvas`; the default is one
  of them — unless the Spore declares `[[surfaces]]`, in which case the default
  names one of those and the kind list is only an outer bound. See
  [Spore surfaces](./spore-surfaces.md).
- Grant keys are `soil:`, `wasi:`, or `fungos:` capabilities with a written
  reason. A grant is reviewed with the pull request.
- Asset media types must be real `type/subtype` strings.
- `docs.man` is a relative `.md` path inside the package; `docs.usage` holds at
  most 8 plain lines.
- `catalog.categories` uses the fixed vocabulary; keywords are lowercase, at
  most 12; screenshots are `ipfs://` references with alt text, at most 10.
  `presentation.icon` and `catalog.record` are the same kind of reference.
- `[[actions]]` declares what the Spore offers to the action bus. Each needs an
  identifier, a title, an input schema, and capabilities that actually parse as
  `soil:`/`wasi:`/`fungos:`.
- `[[skills]]` declares an agent skill by path; its frontmatter `name` must
  match the declaration and its directory, and the description must not be
  empty.
- `[[surfaces]]` declares a view the package ships: a `web` or `canvas` kind, an
  optional entry point that defaults to `web/index.html` or the package's own
  `[ui] entry`, and an optional engine requirement. Declaring none is the
  behaviour every manifest had before surfaces existed.

## Skills

A Spore can ship the agent skill that teaches an agent to use it. Declare each
`SKILL.md` by path, the same way an asset is declared, so the lock and the tar
cover it:

```toml
[[skills]]
name = "bob"
path = "skills/bob/SKILL.md"
```

`name` must match the frontmatter and the directory the file lives in:

```markdown
---
name: bob
description: Use when a build must not land on this machine.
---
```

`soil-spore` rejects a declared skill that is missing, has no `---` frontmatter,
carries a `name` different from the declaration, lives in a directory of another
name, or has an empty description. A man page describes a command to a human; a
skill is how an agent learns when to reach for it. Both belong to the package,
and the skill is the one the registry installs.

## Lock

`spore.lock` is the package identity. It lists every packaged file — see the
tree above — except `signatures/` and itself:

```text
spore-lock 1
spore dev.fungos.markdown
version 0.1.0
file <sha256> <size> assets/site-map.png
file <sha256> <size> module.wasm
file <sha256> <size> spore.toml
file <sha256> <size> ui/editor.lua
digest <sha256 of the lines above>
```

Paths are sorted. `check` refuses a stale lock, so a review sees exactly the
bytes that will ship. The lock does not cover itself, and it does not cover
signatures: re-signing a package must not change its identity.

## Tar and IPFS

`pack` writes a deterministic tar: sorted entries, `mtime` 0, uid/gid 0, fixed
modes, and `spore.lock` included. Repacking the same tree yields identical
bytes, so an IPFS CID identifies the package and nothing else. Publish the tar;
let IPFS compute the CID.

`verify` reads the tar, rejects anything that is not a regular file or that
escapes the root, recomputes the lock from the contents, and refuses a mismatch.
The embedded `spore.lock` makes a package self-describing after download.

## Trust

Soil never generates, stores, interprets, or fetches a key.

```sh
soil-spore sign markdown.spore --key <fingerprint>
soil-spore sign markdown.spore --principal alice [--socket PATH] [--fingerprint FP]
soil-spore verify-signature markdown.spore --keyring <file> [--key <fingerprint>]
```

Signatures are detached, armored, and PGP. `sign --key` shells out to `gpg`;
`sign --principal` asks `geode-pgp`, which leases the key from `geoded` and
returns only the signature. No key material enters this repository or a Spore
package.

Trust is Mycelium's job, not Soil's. Mycelium binds a PGP key to a user and
manages it in Geode, or accepts an external keyring supplied by the operator.
`--principal` is the Geode account label: the reference is
`geode://principal/<label>/pgp`. Soil receives a keyring for verification and,
optionally, a required fingerprint.

A Spore may declare a host-system surface:

```toml
[system]
role = "consent"
```

This is not a grant. The host must allowlist the exact Spore id, role and signed
digest before treating it as a system surface.

Publisher identity is a Geode-backed PGP key. A store operator delegates that
key to a publisher principal; the delegation and the package signature are
verified separately. The signature covers the manifest, lock and module; a
revoked or expired delegated key fails closed. Package trust and capability
grants stay separate.

## Commands

```sh
soil-spore check   <dir> [--publish]     # manifest, declared files, lock freshness
soil-spore lock    <dir>                 # write or refresh spore.lock
soil-spore digest  <dir>                 # package digest (pre-pack)
soil-spore man     <dir|pkg> [--width N] # generated man page
soil-spore info    <dir|pkg>             # listing/identity block
soil-spore pack    <dir> [-o file.spore] # deterministic tar
soil-spore verify  <file.spore>          # tar safety, lock, digests
soil-spore sign    <file.spore> [--key ID | --principal LABEL [--socket PATH] [--fingerprint FP]]
soil-spore verify-signature <file.spore> [--keyring FILE] [--key ID]
soil-spore --version
```

`man` renders the page from the manifest — name, version, licence, launch
surfaces and grants are never retyped — and appends `docs.man` as text.
`--publish` applies the mechanical listing requirements: a description, a
homepage, a synopsis, a manual, an icon, a category, keywords, at least two
screenshots, and a written reason for every grant. Passing it means a package is
ready to be *reviewed*, not approved; see `docs/spore-store.md`.

`check` reads the module's component type and fails when an imported WIT package
is missing from `[grants]`. A module that imports `soil:records` while the
manifest lists only WASI is rejected before anyone signs it. The manifest stays
intent — the host still enforces — but it can no longer be wrong about what the
module asks for. A core module is rejected outright: a Spore module is a
component, and a core module has no component type to check.

## Man pages in Mush

The shell answers for a Spore the way it answers for a command: `man markdown`
prints the page, and `markdown --version` prints the identity line. Both are
reads of installed metadata, not a launch, so they work in a session with no
display.

## Host install

`soil-install` is the host authority, separate from the store presenter:

```sh
soil-install install <file.spore> --root DIR [--keyring FILE] [--key FP]
soil-install rollback --id ID --root DIR
soil-install status --root DIR [--json]
soil-install apply --index FILE --root DIR --packages DIR [--keyring FILE] [--write]
```

`install` verifies the tar and optional publisher signature, caches the tar by
digest, unpacks into staging, and swaps the installed directory. `rollback`
reinstalls the one previous digest. `apply` uses `soil-store plan` and applies
only `live` and `restart`; `consent`, `system` and `hold` are reported, never
applied. A write requires a keyring unless `--allow-unsigned-dev` is explicit.

## Not implemented

- The store Spore's `soil:spore.install` capability. `soil-install` is the host
  CLI; granting that capability to `spores/store` is still a separate step.
- Publishing to IPFS or a mirror set. `pack` produces the bytes; pinning them is
  the publisher's step.
- Revocation. Mycelium owns that record; a package carries no revocation state.
- Signature verification inside the tar; signatures are sidecars by design.
- Guest ABI bindings. `module.wasm` is produced by the Soil guest build; this
  crate checks and packages whatever that build emits.
