# Permissioned lid guest (reversal-8vo7)

This extends the existing Soil `GuestScene`/phone-shell core-Wasm guest and actual
GFX scene-validation/render path. It is **not Canvas**, a new device owner, a
component-model implementation, or automatic panel policy. No display switching.

## Boundary

`fungos-device-hal::lid_transport::LidClient` owns the versioned bounded `lid-v1`
transport, timeouts, error classification and freshness. Soil depends on that
owner crate via relative path; it never parses Debug output or reinterprets raw
polarity. `LidPosition` and `LidFailure` are owner types. The combined provider
must be the newer lid-v1 binary (HAL hash `24923c64814290eb4a74d8262a583096c4a2bc78ff400b7d9837b723fddf6809`).
Parent's explicit socket is `/run/fungos-device-provider`; Soil has **no guessed
default**. Provider deployment on qualified .074/.075 must explicitly retain
`FUNGOS_LID_POLARITY=standard-closed-asserted` (raw false Open, raw true Closed).
Provider retains its existing owner-only socket and lifecycle.

`GuestScene::new` remains import-free. `GuestScene::with_lid` explicitly enables
only two known imports. A separate `Option<LidGrant>` grants observation for that
instance; `None` returns typed Denied before any network access. Host owns socket
path and max-age (CLI 1000ms), never guest. No WASI, raw FD, device path selection,
input injection, keyboard-light control, display control or socket mutation.

`wit/soil-device.wit` defines the semantic versioned capability. This runtime
uses a documented **core-Wasm lowering**, NOT component canonical ABI:

- module `soil:device/lid@0.1.0`, `read(i32 ptr, i32 capacity)->i32 status`
- `log-change()->()` requests a bounded diagnostic of the latest read
- capacity must equal 288; exported memory bounds checked before transport
- 288-byte little-endian record: status u32 at0, position u32 at4, raw u32 at8,
  node-byte-length u32 at12, provider-observed-monotonic u64 at16, age-ms u64 at24,
  up to256 raw Unix node bytes at32 (no terminator; zero padded)
- status:0 success,1 denied,2 unavailable,3 backend,4 ambiguous,5 resynchronizing,
  6 stale,7 invalid-time,8 disconnected,9 protocol
- position:0 unknown,1 unqualified,2 open,3 closed; raw:0 unknown,1 false,2 true
- failures clear observation fields; no cached open/closed after disconnect
- age uses HAL's provider age + conservative request/retention elapsed;
  provider monotonic timestamp is a separate clock, never compared to host clock
- one provider read and one log per guest invocation, in addition to existing
  fuel/memory/scene bounds. HAL bounds each request to2s I/O deadlines. Poll count
  CLI1..600, interval50..10000ms. This is polling, not a promised push stream.

The real `phone-lid.wasm` guest imports read, reads into its memory at4096 on each
`run`, compares status/position/raw to persistent globals and calls log-change
for its initial state and every observed change. Host formats the typed observed
record only when guest requests a log. Unchanged timestamp/age does not spam logs.
The existing synthetic scene remains unchanged and is rendered through GFX each
poll; it does not represent an automatically selected phone display.

## Build / host proof

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p soil-runtime --bin soil-lid -- --export-guest target/phone-lid.wasm
cargo run -p soil-runtime --bin soil-lid -- --guest target/phone-lid.wasm --socket /nonexistent --deny-lid 2 100
cargo run -p soil-runtime --bin soil-lid -- --guest target/phone-lid.wasm --socket /nonexistent --allow-lid 2 100
CARGO_BUILD_JOBS=2 cargo zigbuild -p soil-runtime --bins --release --target aarch64-unknown-linux-musl
```

Host integration tests execute the actual guest against typed HAL codec Unix
socket fixtures: initial open, initial closed without motion, live close,
provider loss, unchanged-failure suppression, reconnect/open, stale, unqualified,
resynchronizing, provider denial, permission denial with no socket contact,
unrelated imports denied and out-of-bounds buffer rejection. WIT syntax tested.
These tests do not certify physical phone execution or hardware polarity.

## Parent RAM-only deployment

Artifacts:
- `target/aarch64-unknown-linux-musl/release/soil-lid` (static ARM64)
- `target/phone-lid.wasm` (real core-Wasm module, exported by host CLI)

First retain the baseline JIT proof. .074 kernel panics in Wasmtime icache flush
with errno38 ENOSYS; parent is enabling CONFIG_MEMBARRIER for .075. No safety
bypass is made. .075 phone execution is not claimed by host builds.

After staging both files in `/tmp`, and deploying the newer combined HAL binary
through its existing lifecycle/config, parent runs:

```sh
/tmp/soil-lid --guest /tmp/phone-lid.wasm --socket /run/fungos-device-provider --deny-lid 2 250
/tmp/soil-lid --guest /tmp/phone-lid.wasm --socket /run/fungos-device-provider --allow-lid 120 250
```

Denied logs once with position/source/freshness unknown. Allowed logs initial
open/closed immediately without requiring motion; flip physically while running
for live changes. Parent stops/restarts existing provider while guest continues:
expect typed unknown on disconnect then a new fresh state on reconnect. Repeat
startup held open and held closed separately. Capture stdout/stderr and hashes;
no guest auto display actions. No phone, shared kernel, commit or push operations
are performed by this worker.
