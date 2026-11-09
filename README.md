# Soil

**Soil is the WASM runtime for fungOS apps.** Applications that run on Soil are
called **Spores**: a Spore is a signed manifest (module CID, capabilities, target,
engine version) plus its module bytes. Distribution is planned as the **Spore
Store** — publish by CID, resolve by CID + target, revoke, never re-sign.

Status: **early.** The full runtime host is not implemented; the first
capability host (BLE) and one real Spore do run. Nothing here is a shipped
product surface yet. Canonical capability contracts for `soil:view`,
`soil:time`, `soil:storage`, `soil:ble` and `soil:canvas` live in [`wit/`](wit)
and are enforced by `crates/soil-abi`. Canvas is the cross-platform owner;
`soil:surface` is deferred. The broader design record lives in the `mycelium`
repository (`docs/soil-capabilities.md`, PR pending) and covers the remaining
capability boundary (`soil:gpu`, `soil:audio`, `soil:stream`),
signed-Spore verification, and the target matrix (macOS, Linux, Android/Fire TV,
CAT S22; tvOS is AOT-only because it forbids JIT).

The first concrete workload is the [Viewer Spore](docs/viewer-spore.md): one
presentation viewer for web, signed Spores and retained scenes, with normalized
remote actions and bounded host capture. It is designed for the existing Canvas
Fire TV app first, then the extracted Soil runtime.

A second spike is the [Feeds Spore](docs/feeds-spore.md): a reader that consumes
RSS/Atom items from `soil:records`, keeps its private settings in
`soil:appdata`, and leaves fetching to a host-side poller with a dotfile feed
list.

Spores do not import Unibus or Iroh. The host owns the shared
`unibus/peer/1` session, and application sync protocols are message kinds on
that session. The Iroh listener/router lives in `crates/soil-sync-host`; a real
encrypted loopback test syncs records through it. See
[Shared peer sync](docs/peer-sync.md).

Principles:

- **No ambient authority.** A Spore runs against exactly the imports its signed
  manifest was granted; there is no filesystem, network, clock or device access
  beyond that.
- **Transport in Soil, protocol in the Spore.** Radios and GPUs are host
  capabilities; wire formats and state machines ship as `.wasm`.
- **Honest failure.** A rejected or unverifiable Spore fails loudly with a named
  reason, never silently degrades.

## Capability contracts

| Package | Grant | Status |
|---|---|---|
| [`soil:principal@0.1.0`](wit/soil-principal.wit) | `soil:principal` | host-attested Mycelium/OIDC projection; actor, device, teams, roles |
| [`soil:view@1.0.0`](wit/soil-view.wit) | `soil:view`, optional `soil:view.capture` | WIT + Viewer state machine |
| [`soil:time@1.0.0`](wit/soil-time.wit) | `soil:time` | WIT |
| [`soil:storage@1.0.0`](wit/soil-storage.wit) | `soil:storage.read`, optional `soil:storage.write` | WIT |
| [`soil:appdata@1.0.0`](wit/soil-appdata.wit) | `soil:appdata` | WIT + guest SDK + records-backed host |
| [`soil:records@0.1.0`](wit/soil-records.wit) | `soil:records` read/write/sync | WIT + generic/contacts host + guest SDK |
| [`soil:http@0.1.0`](wit/soil-http.wit) | `soil:http` | WIT + bounded host + direct transport proof |
| [`soil:index@1.1.0`](wit/soil-index.wit) | `soil:index` | derived browse/search + explicit KNN; fast direct-child traversal |
| [`soil:vfs-mut@0.1.0`](wit/soil-vfs-mut.wit) | `soil:vfs-mut` | separate write/mkdir/rmdir/rename grant |
| [`soil:media@0.1.0`](wit/soil-media.wit) | `soil:media`, optional `soil:media-encoded` | host playback, encoded chunks, bounded image readback |
| [`soil:surface-import@0.1.0`](wit/soil-surface-import.wit) | `soil:surface-import` | host-owned imported surfaces, damage events, normalized input |
| [`soil:inference@0.2.0`](wit/soil-inference.wit) | `soil:inference` | WIT + models, tools, completion, SSE stream resource |
| [`soil:search@0.6.0`](wit/soil-search.wit) | `soil:search` | WIT + files/records/graph host composition + host-local application inventory + typed `@` mention resolver |
| [`soil:spore@0.1.0`](wit/soil-spore.wit) | `soil:spore.catalog`, `soil:spore.launch` | installed-Spore catalog and host-owned launch; listing and running are separate intents |
| [`soil:graph@0.1.0`](wit/soil-graph.wit) | `soil:graph` | read-only edge/reference queries over records |
| [`soil:input@1.0.0`](wit/soil-input.wit) | — | shared normalized input types for Canvas, Terminal and imported surfaces |
| [`soil:notify@1.1.0`](wit/soil-notify.wit) | `soil:notify`, `soil:notify/ringer` | WIT + host broker + notify Spore; ringer is an interface, not a package |
| [`fungos:actions@1.0.0`](wit/fungos-actions.wit) | exact action id/version grants | granted catalog + bounded invoke; Canvas uses the catalog projection |
| [`soil:device@0.1.0`](wit/soil-device.wit) | `soil:device` | semantic battery/estimate/lid contracts; runtime uses core-Wasm lowering |
| [`soil:ble@1.0.0`](wit/soil-ble.wit) | `soil:ble.observe`, `soil:ble.connect`, `soil:ble.write` | WIT + host + COLMI ring Spore |
| [`soil:canvas@1.0.0`](wit/soil-canvas.wit) | `soil:canvas` | WIT + input normalizer + S22 runtime + Android JNI |
| [`wasi:webgpu@0.3.0-rc.2`](crates/soil-webgpu/wit/deps/wasi-webgpu-0.3.0-rc.2/package.wit) | link `wasi:webgpu` | upstream WIT + host + compute fixture |
| [`wasi:audio@0.1.0`](wit/wasi-audio.wit) | link `wasi:audio` | draft WIT + cpal host + tone fixture |

`cargo test -p soil-abi` parses every canonical package and asserts its reviewed
name, interface and functions, so an accidental ABI change fails the build. See
[`wit/README.md`](wit/README.md) for the canonical/contract/deferred split.

Battery, estimate, lid and Canvas animation are semantic WIT but are lowered
through a bounded core-Wasm ABI by the S22 runtime. See
[`docs/abi-core-wasm.md`](docs/abi-core-wasm.md).

The first host-backed capability is BLE: [`crates/soil-ble-host`](crates/soil-ble-host)
runs the [COLMI ring Spore](https://github.com/ajmwagar/spores/tree/main/spores/colmi-ring)
against a real radio on desktop.

Canvas is the cross-platform owner; Spores never submit UI trees.
[`crates/soil-runtime`](crates/soil-runtime) is the S22 reference host (wasmtime
core runner, Canvas/battery/lid/estimate capabilities), and
[`crates/soil-android`](crates/soil-android) is the JNI bridge used by the Canvas
Android app. The portable
WebGPU host is [`crates/soil-webgpu`](crates/soil-webgpu): raw
`wasi:webgpu` components and real wgpu 29 programs both request a GPU
adapter/device, run WGSL compute and render passes, read results back through
`wgpu-core`, and hand a standard WebGPU texture to a host-owned Canvas frame
through the narrow `soil:canvas-gpu` presentation bridge. See [`docs/soil-webgpu.md`](docs/soil-webgpu.md).
The draft audio sink is [`crates/wasi-audio-wasmtime`](crates/wasi-audio-wasmtime):
a real component pushes bounded PCM into a cpal host. See
[`docs/wasi-audio.md`](docs/wasi-audio.md). Guest code uses the umbrella
[`crates/soil-sdk`](crates/soil-sdk); its first feature, `audio`, provides
`Output`, `write_all`, `tone` and `play_tone`. See
[`docs/soil-sdk.md`](docs/soil-sdk.md).
The CRM is a Spore over namespaced records, not a host: [`spores/crm`](spores/crm)
reads `crm`, `contacts`, `calls` and `messages` namespaces, while the host maps
those names to books and enforces grants. See [`docs/crm.md`](docs/crm.md).
Personal contact editing and explicit first-name/token sharing live in a
separate [`spores/contacts`](spores/contacts); see [`docs/contacts.md`](docs/contacts.md).
The reusable WASI p2 profile is [`crates/soil-wasi-host`](crates/soil-wasi-host):
`wasi:http` incoming/outgoing, `wasi:sockets` with per-use CIDR policy, clocks,
random, filesystem preopens and a per-request store. `soil-wasi-http` serves a
component's `wasi:http/incoming-handler` with a deny-by-default origin/network
policy. See [`docs/wasi.md`](docs/wasi.md).
The portable Linux/Android/macOS input normalizer is
[`crates/soil-canvas-input`](crates/soil-canvas-input);
[`docs/soil-canvas.md`](docs/soil-canvas.md) records the decision and host
obligations, [`docs/soil-canvas-s22.md`](docs/soil-canvas-s22.md) records the S22
proving run, and [`docs/android.md`](docs/android.md) records the first Android
JNI proof.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p viewer-spore --target wasm32-wasip2
cargo check --manifest-path ../ajmwagar/spores/spores/colmi-ring/Cargo.toml \
  --target wasm32-wasip2
```

### One build cache, one installed host

The hosts link Wasmtime. A private `target/` per worktree therefore costs several
gigabytes each, and a machine running several worktrees at once fills its disk
long before it runs out of source. Build once and share:

```sh
export CARGO_TARGET_DIR="$HOME/Library/Caches/soil/cargo-target"
export CARGO_INCREMENTAL=0
cargo install --path crates/soil-search-host --bin soil-search-web \
  --root "$HOME/.local"
```

Agents and Spores then run `soil-search-web` from `~/.local/bin` against whatever
`target/` the shared cache already holds. Rebuilding is a soil decision, not a
Spore decision: the WIT packages and the hosts that implement them move together
(`soil:search@0.6.0`, `soil:index@1.1.0`), so a change to `wit/` or a host crate
means a rebuild and a reinstall, while work in `spores/` or a canvas worktree does
not.

The capability contracts parse as WIT, and the Viewer state machine has no host
authority.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

The threaded process lane is documented in [`docs/soil-process.md`](docs/soil-process.md). Ordinary cpal callback programs can run there with a guest-owned audio thread; the host device callback never enters Wasm. The only app line is `let host = cpal_soil::host();` on both native and threaded Wasm.

The first CID-neutral content/index slice is [`crates/fungos-content`](crates/fungos-content). See [`docs/fungos-content.md`](docs/fungos-content.md).

## Dependencies

Builds use crates.io, public git revisions, and packages inside this checkout.
No sibling worktrees are required. See [dependency policy](docs/dependencies.md).

## Shell

`soil shell` mounts a VFS through FUSE and starts an interactive shell. `soil rm` unlinks the namespace entry while retaining the content-addressed blob and history. See [Soil shell](docs/soil-shell.md).

## Soil operator config

Operator inference profiles and `unibus-mcp`/DCP routes resolve from one Soil config instead of scattered provider environment. See [Soil operator and inference config](docs/soil-config.md).

## Scrubber

`soil-scrub` is a local reversible redactor for prompts, tool calls, and tool output. It uses keyed session tokens, not hashes, and restores values only for explicitly scoped host tools. See [Scrubber](docs/scrubber.md).

## CRM import

`soil-crm-host` provides a deterministic mock CRM and a local team/private inspector; `soil-crm-espo` is the deferred EspoCRM importer. See [CRM](docs/crm.md) and [EspoCRM import](docs/crm-espo.md).

## Mush

`mush` is now a Wasm Spore over `soil:vfs`, `soil:index`, and `soil:vfs-mut`: no host PATH, no subprocesses, workspace-relative paths, `.mushrc`, `PWD`/`OLDPWD`, `TERM`, `mkdir`, `rmdir`, and `rm` as namespace unlink. See [Mush](docs/mush.md).
