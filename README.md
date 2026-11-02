# Soil

**Soil is the WASM runtime for fungOS apps.** Applications that run on Soil are
called **Spores**: a Spore is a signed manifest (module CID, capabilities, target,
engine version) plus its module bytes. Distribution is planned as the **Spore
Store** — publish by CID, resolve by CID + target, revoke, never re-sign.

Status: **early.** The runtime host is not implemented; nothing here is a
shipped product surface yet. Canonical capability contracts for `soil:view`,
`soil:time` and `soil:storage` live in [`wit/`](wit) and are enforced by
`crates/soil-abi`. The broader design record lives in the `mycelium` repository
(`docs/soil-capabilities.md`, PR pending) and covers the remaining capability
boundary (`soil:ble`, `soil:gpu`, `soil:audio`, `soil:stream`, `soil:surface`),
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

`cargo test --workspace` parses every package and asserts its reviewed name,
interface and functions, so an accidental ABI change fails the build.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p viewer-spore --target wasm32-wasip2
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
