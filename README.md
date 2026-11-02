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
| [`soil:view@1.0.0`](wit/soil-view.wit) | `soil:view`, optional `soil:view.capture` | WIT + Viewer state machine |
| [`soil:time@1.0.0`](wit/soil-time.wit) | `soil:time` | WIT |
| [`soil:storage@1.0.0`](wit/soil-storage.wit) | `soil:storage.read`, optional `soil:storage.write` | WIT |
| [`soil:ble@1.0.0`](wit/soil-ble.wit) | `soil:ble.observe`, `soil:ble.connect`, `soil:ble.write` | WIT + host + COLMI ring Spore |
| [`soil:canvas@1.0.0`](wit/soil-canvas.wit) | `soil:canvas` | WIT + input normalizer + S22 runtime + Android JNI |
| [`wasi:webgpu@0.3.0-rc.2`](crates/soil-webgpu/wit/deps/wasi-webgpu-0.3.0-rc.2/package.wit) | link `wasi:webgpu` | upstream WIT + host + compute fixture |
| [`wasi:audio@0.1.0`](wit/wasi-audio.wit) | link `wasi:audio` | draft WIT + cpal host + tone fixture |

`cargo test --workspace` parses every package and asserts its reviewed name,
interface and functions, so an accidental ABI change fails the build.

The first host-backed capability is BLE: [`crates/soil-ble-host`](crates/soil-ble-host)
runs the [`spores/colmi-ring`](spores/colmi-ring) protocol Spore against a real
radio on desktop. See [`spores/colmi-ring/README.md`](spores/colmi-ring/README.md).

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
cargo check --manifest-path spores/colmi-ring/Cargo.toml --target wasm32-wasip2
```

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
