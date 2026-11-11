# Lua extensions

Status: **proposal.** Answers the question "can every Spore be pluggable in Lua
the way `gpui-shell` is pluggable in JavaScript", and what that means for
composition and ownership.

## Two trust domains, one language

There are two different products behind "Lua plugins", and they must not be
built on the same trust model:

| | in-app Lua | Spore Lua |
|---|---|---|
| who wrote it | the app's own users/team | anyone |
| where it runs | same process, same heap as the host | inside a wasm component |
| isolation | capability handles (`cap-std`-style) | wasm + WIT grants |
| reload | file watch, no build | rebuild/stream the component |
| UI | builds the host's elements (GPUI) | presents a texture/scene through `soil:surface` |

The same Lua source should run in both, which is only possible if the Lua API
never exposes host handles. A Lua widget layer (`text`, `button`, `row`, state,
events) can be implemented twice: over GPUI in-process, and over the guest
platform in a Spore. What cannot be shared is the host's `Window`, `App`, GPU
device, or file descriptors.

This is also the honest answer to "does it have to be GPUI":

- **In-app**, yes, if you want one element tree and no IPC — that is exactly
  `gpui-shell`'s trick, with Lua instead of QuickJS.
- **In a Spore**, no: Lua must target a contract that leaves the component.
  Since first-party Spores are GPUI, the guest can embed Lua + GPUI and keep the
  no-IPC property inside the Spore; the WIT boundary stays outside.

## Composition between Spores

Status: **deferred.** The mechanisms below are the design space, recorded so
the decision does not have to be rediscovered; none of them is adopted yet.

Never a shared Lua VM across Spores. A shared VM is a shared fault domain, a
shared capability domain, and a shared upgrade problem. Composition is one of:

1. **Packages (build time).** Lua modules are versioned dependencies, pinned by
   hash in `spore.lock` (which already carries `file <sha256> <size> <path>`).
   Every Spore loads its own copy into its own VM. Reuse happens when the Spore
   is built, not when it runs.
2. **Host-mediated data (run time).** Spores compose through capabilities —
   `soil:appdata`, `soil:records`, `soil:unibus`, `soil:vfs` — or by publishing
   and consuming events. No direct call from one Spore's Lua into another's.
3. **UI composition by the host.** Each Spore presents a texture (or scene) into
   a region; Canvas composites them and routes input by region. That is the
   whole point of the compositor split.

If two Lua files must share a value, the value travels as data through a
capability with a schema, not through a global.

## Who owns edits

A layered override chain, every layer with an owner and a provenance record:

| layer | owner | signed | persisted |
|---|---|---|---|
| `shipped` | the Spore author | yes | with the Spore |
| `distribution` | org/brand/admin | yes | with the distribution |
| `user` | the person running it | no | per-user data dir / Spore appdata |
| `session` | dev, `tend watch` | no | never |

Rules that keep this from becoming a fork farm:

- **Overrides are patches, not copies.** A user layer overrides or wraps a named
  definition (`local base = require("app.view"); return function(props) … end`)
  so an upstream update still applies. A copied-out file is a fork and must be
  marked as one so upgrades can warn instead of silently diverge.
- **Capabilities belong to the Spore, not the layer.** A user layer cannot
  import anything the Spore's manifest did not grant. The host enforces this at
  the WIT boundary; a layer never gets host authority.
- **Shared layers are signed and hash-pinned.** User layers are local and
  untrusted, and only ever run in whichever domain (in-app or Spore) the app
  chose for them.
- **Provenance is visible.** The host keeps which layer defined what, so `tend`
  can show it, and "reset to default" is a single operation.

## How a real feature arrives

Lua is for config, layout, glue and small logic. A new capability is Rust:

- **New Spore** — a wasm component added to the app and composed by the host.
  Its manifest carries its own grants; the app is not rebuilt.
- **App update** — when the feature needs host-side code (window, GPU, files,
  system APIs), the app ships again and exposes a new capability.

The shim is WIT. Rust code below it, Lua above it, each hop a typed call. That
is the "Rust ↔ Lua ↔ Rust" path without a shared ABI.

## What this needs next

1. A small `gpui-lua` binding for the in-app case: one Lua state per extension,
   a widget vocabulary (`text`, `button`, `row`, state, events), file-watch
   reload. It should be able to run today's `ui.lua`/`components` vocabulary.
2. A layer loader in `tend` (shipped → distribution → user → session) with
   provenance, reusing its existing check/watch loop.
3. The guest variant once the GPUI-on-wasip2 path lands (same critical path as
   the GPU-spore probes).
