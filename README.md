# Soil

**Soil is the WASM runtime for fungOS apps.** Applications that run on Soil are
called **Spores**: a Spore is a signed manifest (module CID, capabilities, target,
engine version) plus its module bytes. Distribution is planned as the **Spore
Store** — publish by CID, resolve by CID + target, revoke, never re-sign.

Status: **early.** The runtime host and its capability ABI are being designed
before implementation; nothing here is a shipped product surface yet. The current
design record lives in the `mycelium` repository (`docs/soil-capabilities.md`,
PR pending) and covers the capability boundary (`soil:ble`, `soil:gpu`,
`soil:audio`, `soil:stream`, `soil:surface`), signed-Spore verification, and the
target matrix (macOS, Linux, Android/Fire TV, CAT S22; tvOS is AOT-only because it
forbids JIT).

Principles:

- **No ambient authority.** A Spore runs against exactly the imports its signed
  manifest was granted; there is no filesystem, network, clock or device access
  beyond that.
- **Transport in Soil, protocol in the Spore.** Radios and GPUs are host
  capabilities; wire formats and state machines ship as `.wasm`.
- **Honest failure.** A rejected or unverifiable Spore fails loudly with a named
  reason, never silently degrades.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
