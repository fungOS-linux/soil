# Spore packages

A Spore is a module (WASM), a UI (Lua), and a manifest that names every asset
and grant. `soil-spore` builds and verifies the package. It does not publish,
host, or resolve anything.

```text
markdown/
  spore.toml              identity, engine, grants, launch, assets
  module.wasm             logic
  ui/editor.lua           presentation (Canvas JITs it)
  assets/site-map.png     content named by the manifest
  signatures/<keyid>.asc  detached PGP, excluded from the lock and the tar
  spore.lock              generated, included in the tar
```

Distribution is a **tar over IPFS**. Soil does not own an archive format, a
registry, or a key server.

## Manifest

```toml
schema = 1

[spore]
id = "dev.fungos.markdown"   # stable reverse-DNS
name = "markdown"
version = "0.1.0"
license = "MIT"
summary = "Markdown reader/editor with a hot-reloadable Lua UI"

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

[grants]
"soil:canvas" = "present the reader/editor surface"

[assets]
"site-map" = { path = "assets/site-map.png", media_type = "image/png" }
```

Validation is strict because a package is executable content:

- `id` is a stable reverse-DNS identifier; `version` is semver-ish.
- Every path is relative, has no `..`, no backslash, no control characters, and
  no symlink. `module.wasm` must end in `.wasm`; the UI entry in `.lua`.
- `launch.surfaces` is a subset of `web`, `native`, `canvas`; the default is one
  of them.
- Grant keys are `soil:`, `wasi:`, or `fungos:` capabilities with a written
  reason. A grant is reviewed with the pull request.
- Asset media types must be real `type/subtype` strings.

## Lock

`spore.lock` is the package identity. It lists every file except
`signatures/` and itself:

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
soil-spore verify-signature markdown.spore --keyring <file> [--key <fingerprint>]
```

Signatures are detached, armored, and PGP. `sign` and `verify-signature` shell
out to `gpg`; no key material enters this repository or a Spore package.

Trust is Mycelium's job, not Soil's. Mycelium binds a PGP key to a user and
manages it (for example in Geode), or accepts an external keyring supplied by
the operator. Soil receives a keyring and, optionally, a required fingerprint.
If Mycelium later supplies keys through a resolver, only the argument to
`verify-signature` changes.

## Commands

```sh
soil-spore check   <dir>                 # manifest, declared files, lock freshness
soil-spore lock    <dir>                 # write or refresh spore.lock
soil-spore digest  <dir>                 # package digest (pre-pack)
soil-spore pack    <dir> [-o file.spore] # deterministic tar
soil-spore verify  <file.spore>          # tar safety, lock, digests
soil-spore sign    <file.spore> [--key ID]
soil-spore verify-signature <file.spore> [--keyring FILE] [--key ID]
```

## Not implemented

- Publishing to IPFS, a Spore Store, or a mirror set.
- Revocation. Mycelium owns that record; a package carries no revocation state.
- Signature verification inside the tar; signatures are sidecars by design.
- Guest ABI bindings. `module.wasm` is produced by the Soil guest build; this
  crate checks and packages whatever that build emits.
