# Permissioned battery Canvas — reversal-ozgo.5

## Frozen artifact receipt

Base: `/Users/ajmwagar/Documents/src/fungOS-linux/soil/.worktrees/s22-runtime`

- `target/aarch64-unknown-linux-musl/release/soil-canvas`: static ARM64 ELF,
  SHA-256 `ebd457020d58db2d933fb2857a25ea308d2731309e9e790689cc916bbf02f826`
- `target/battery-canvas.wasm`: actual core-Wasm guest, 325 bytes,
  SHA-256 `f2da070c80d60f0426d77c3f8d368ba325c5d9a6a4bfed6f25d705640862214f`

Artifacts frozen; no further rebuild performed for this receipt.

## Capability boundary

`GuestScene::with_battery_canvas` explicitly links
`soil:device/battery@0.1.0`; a separate optional `BatteryGrant` authorizes HAL
access. No grant returns Denied before transport. The host selects the trusted
socket and max age (CLI 1000ms); no guest device discovery or socket authority.
HAL owns bounded parsing, transport deadlines, and provider-age + RTT + retention
freshness through `RemoteBatterySample::snapshot`.

`wit/soil-device.wit` is the semantic contract. This implementation uses a
core-Wasm lowering, not component canonical ABI:

- `read(i32 ptr, i32 capacity) -> i32`: capacity exactly 24; exported-memory
  bounds validated before transport; success returns 0.
- 24-byte little-endian record: opaque token i64 at 0, availability u32 at 8
  (0 Fresh, 1 Unknown, 2 Stale, 3 Denied), presence flags u32 at 12
  (bit0 percent, bit1 voltage), percent u32 at 16, voltage microvolts u32 at 20.
  Absent slots are zero padding, **not numeric observations**.
- `update-status(i64 token) -> i32`: consumes the invocation-scoped host token;
  guest numeric memory is never trusted as device data. Invalid/replayed tokens
  trap. Host-retained snapshot freshness is rechecked at consumption and again
  immediately before every presentation.
- At most one read and one update per invocation; frame/input reset invalidates
  pending tokens and committed data. Fuel and existing Canvas budgets remain.
- No read/update means no retained fresh numbers at presentation. Every failure
  clears numbers; Denied/Stale retain those availability labels, other typed HAL
  failures become Unknown. No charging/current/temperature UI fields exposed.

`battery-canvas.wat` is a separate actual guest requesting read, relaying the
opaque token, and presenting through the existing Canvas owner. Existing guest,
constructors, imports, default Canvas, and no-battery CLI behavior remain intact.

## CLI

Host export (not ARM execution):

```sh
cargo run -p soil-runtime --bin soil-canvas -- \
  --export-battery-guest target/battery-canvas.wasm
```

Parent target command, with parent-selected staging/output paths:

```sh
/run/soil-canvas --guest /run/battery-canvas.wasm \
  --output /tmp/battery-preflight.bgr --frames 1 --interval-ms 250 \
  --ui simple-jarvis --battery-socket /run/fungos-battery-provider.sock \
  --allow-battery
```

Use `--deny-battery` for denial. Append `--surface outer` for 128x128; default
inner is 480x640. Retain production output paths, timing and keypad flags when
replacing producers. No presenter, lid, DRM, provider lifecycle or panel-policy
changes are part of this implementation.

## Local verification

- Final full workspace rerun: **47 tests passed**, zero failures
  (19 + 6 + 5 + 1 + 4 + 8 + 4). This is the measured count, not 49.
- Battery integration suite: **6 passed**, including the final native-pixel test.
- Final strict workspace Clippy passed. Workspace check passed before the final
  tests. Soil files formatted directly; `cargo fmt --all` is blocked by an
  unrelated external dependency workspace configuration.
- Actual Wasm tests cover unknown percent/real voltage, genuine zero, denial
  with no socket contact, malformed/disconnected provider and recovery, stale,
  invalid memory/token/replay, numeric tampering, and bounded calls.
- Native owner pixel test verifies opaque nonuniform output at both 480x640 and
  128x128 through the battery guest. Separate retention test checks expiry.
- Static build succeeded; dependency cdylib and deprecated linker-setting
  warnings only. Logs: `/tmp/soil-battery-tests.log`,
  `/tmp/soil-battery-final-tests.log`, `/tmp/soil-battery-build.log`.

## Parent-reported live receipt

Parent reports frozen executable `ebd4570…` and guest `f2da07…` deployed RAM-only
under `/run`. Actual guest preflight passed at both frame sizes. Both producers
were replaced with explicit battery grants and 1000ms freshness, while presenters
PIDs 253/229 and lid PID 230 stayed unchanged; RO1 retained. HAL reported fresh
voltage **3892194 microvolts**, percent **unknown**, not zero.

Parent captured target outer frame `/tmp/cat091-battery-outer-live.png`, reporting
589 distinct colors (nonuniform). OCR was blocked by an environment/private-path
issue; no OCR success is claimed. This worker did not operate the phone.

Live physical lid-close validation remains pending the parent/user physical
interaction. No automatic close/display switching was introduced or claimed.
