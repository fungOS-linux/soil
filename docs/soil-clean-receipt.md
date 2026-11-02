# Clean voltage-only binary receipt

Verified completed separate-target build, no rebuild for this verification.

Artifact (relative to Soil s22-runtime worktree):
`target/soil-canvas-clean/aarch64-unknown-linux-musl/release/soil-canvas`

- file: ELF64 ARM aarch64, statically linked, stripped
- SHA256: `a40f913693739721afb72305bc1eaaea5f112e5fc579542d207808f974ddbb88`
- original executable unchanged: `ebd457020d58db2d933fb2857a25ea308d2731309e9e790689cc916bbf02f826`
- existing `target/battery-canvas.wasm` and clean-build generated battery guest
  both SHA256 `f2da070c80d60f0426d77c3f8d368ba325c5d9a6a4bfed6f25d705640862214f`

Latest source workspace53tests/check/strictClippy passed before build. One prior
run had two lid fixture failures; both passed in full rerun without lid edits.
No subsequent source build changes or hardware verification claimed here.

Build warnings are nonfatal: musl drops an unsupported dependency cdylib output
(the executable's library dependency still compiled); linker ignores deprecated
optimization setting1. Neither indicates dynamic linkage or a failed link.
Actual `file` result confirms the requested static executable. No warning bypass
or security setting change was made. Log `/tmp/soil-canvas-clean-build.log`.

Parent stages clean executable at `/run/soil-canvas-clean`, retains existing
battery guest at `/run/battery-canvas.wasm`, and preflights:

```sh
/run/soil-canvas-clean --guest /run/battery-canvas.wasm \
  --output /tmp/soil-clean-inner.bgr --frames 1 --interval-ms 250 \
  --ui simple-jarvis --battery-socket /run/fungos-battery-provider.sock \
  --allow-battery --surface inner

/run/soil-canvas-clean --guest /run/battery-canvas.wasm \
  --output /tmp/soil-clean-outer.bgr --frames 1 --interval-ms 250 \
  --ui simple-jarvis --battery-socket /run/fungos-battery-provider.sock \
  --allow-battery --surface outer
```

Output atomic BGR24: inner480x640=921600 bytes; outer128x128=49152 bytes.
For producer replacement retain parent's existing frame paths, loop/timing and
keypad grant flags; do not point presenters at preflight files inadvertently.

Existing battery-v1 provider/socket identity unchanged by Soil. Parent previously
reported provider hash `938bb6c4b1c7ebc55b7fa54167617cd2fb23e8d1da95a247e1fe77bab3c2e508`
and socket `/run/fungos-battery-provider.sock`; no live revalidation performed by
this worker. No provider/presenter/lid/DRM lifecycle changes. Existing battery
guest selects battery constructor, not estimate constructor: model channel stays
disabled, no estimate socket or trusted allowlist configured, no fixture output.
BatterySample.percent remains kernel-capacity-only; unreported percent stays
unknown while voltage remains available. Parent alone deploys and owns hardware.
