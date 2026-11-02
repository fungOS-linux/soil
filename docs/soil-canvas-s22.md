# Actual Canvas through Soil on S22

This is the S22 reference-host record. The canonical contract is
[`wit/soil-canvas.wit`](../wit/soil-canvas.wit); the runtime lives in
[`crates/soil-runtime`](../crates/soil-runtime).

## Actual owner and explicit toolkit exception

Canvas repo is `fpl/dock`. Main desktop uses iced; its schema is
`canvas-workspace`/`canvas-widgets`. Earlier implemented owner UI exists in
`fpl/dock/.worktrees/soil-webgpu/crates/canvas-workspace-ui`: egui `Inbox::show`,
validated owner `Snapshot`, owner `demo::work`, Coherence native egui tokens,
and owner keyboard/detail navigation with narrow480px layout tests.

Parent explicitly permits reusing this existing egui owner route despite older
`dock/AGENTS.md` iced wording, rather than inventing a second iced implementation.
The GPU worker's `reversal/.worktrees/s22-gpu-ui/cat-s22/tools/canvas-surface`
crate invokes those actual owner widgets and a third-party CPU mesh/font raster
backend (`egui_software_backend=0.0.2`, egui0.33.3). **CPU**, not GPU acceleration.
This is not the prior geometric GFX fixture labeled Canvas, nor the full desktop
shell. It is actual Canvas Work Inbox with prominently labeled **DEMO DATA**,
not live projects or device observations. Coherence owner theme is reused.

## Soil boundary

- `wit/soil-canvas.wit`: `soil:canvas@0.1.0`, interface `surface`
- `crates/soil-runtime/src/canvas.rs`: host-selected actual owner surface
- `fixtures/canvas-client.wat`: real Wasm imports `present`, `navigate`
- `GuestScene::with_canvas`: explicit per-instance capability grant
- `GuestScene::render_canvas`: returns only frame retained by owner import

No duplicate widget schema: guest requests a granted surface, not pixels or
primitives. No WASI, filesystem path, DRM FD, device reader, socket selection or
raw framebuffer grant. Default constructor remains import-free; lid and Canvas
imports are separate grants. One present/action per invocation plus fuel/memory
bounds. Owner frames must match the host-selected `SurfaceSize`: **Inner480x640**
or **Outer128x128**, exact RGBA length and wholly opaque. The default trait extent
is Inner; `OwnerSurface::with_size` explicitly selects either bounded extent.
The guest cannot choose dimensions or obtain a second surface.

Core-Wasm lowering (not component canonical ABI):
`soil:canvas/surface@0.1.0.present: ()->i32` and
`navigate: (i32)->i32`;0 success,1 denied,2 invalid, backend errors trap.
Keys match Soil normalized ABI1Up2Down3Left4Right5Select6Back.
The guest `run` calls present; `input` forwards normalized events to owner.

## Presenter ownership and initial visible frame

Existing `cat-drm-present` remains the sole DRM owner. Soil publishes tightly
packed BGR24 (Inner480x640,1440rowbytes,921600total; Outer128x128,
384rowbytes,49152total) through same-directory temporary
write/close+atomic rename. Never modify/truncate the published inode. Initial
valid frame must exist before parent starts presenter; subsequent watch errors
preserve last good frame. No promise of tear-free page flipping.

```sh
cargo check --workspace --all-targets
# fmt all traverses an existing external vendor-workspace inconsistency;
# format only Soil-owned packages, not unrelated owner manifests:
cargo fmt -p soil-runtime -p viewer-spore -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 cargo zigbuild -p soil-runtime --bin soil-canvas --release --target aarch64-unknown-linux-musl
```

Parent RAM-only commands (parent stages static ARM64 binary first):

```sh
/run/soil-canvas --export-guest /run/canvas-client.wasm
/run/soil-canvas --guest /run/canvas-client.wasm --output /run/canvas.bgr --frames 6000 --interval-ms 250
# Other shell, only after startup=ready and921600-byte valid frame:
/run/cat-drm-present /dev/dri/card1 watch 100 bgr888 /run/canvas.bgr
```

CLI bounded frames1..36000, interval50..5000ms. Optional **last** arguments
`--surface inner|outer` select the host grant; omitted defaults to inner. Normal exit retains last valid
frame; only parent starts/stops presenter or decides display policy. No automatic
lid/panel switching. Parent physical proof is required; host builds cannot claim
phone readability, navigation, startup/exit display behavior or live data.

## Native128 responsive replacement (current)

Current static stripped ARM64: **`target/soil-canvas-native128`** SHA256
`787bd259c54de21ce66d9aae2a49eeb6641e48fbec081c427491320270e58262`.
This replaces the tiny-font outer output below; same guest and CLI:

```sh
/run/soil-canvas-native128 --guest /run/canvas-client.wasm --output /run/canvas-outer.bgr --frames 6000 --interval-ms 100 --surface outer
/run/soil-canvas-native128 --guest /run/canvas-client.wasm --output /run/canvas.bgr --frames 6000 --interval-ms 100 --keypad-socket /run/fungos-device-provider --allow-keypad --surface inner
```

GPU owner implemented responsive <=160px branch **inside actual Inbox::show**,
after its existing filtering/reconciliation/key handling. Selected title (two
bounded lines), real status, position or owner on details use existing state and
actions, not duplicate UI state/action logic. Native128 logical viewport and
raster scale1 preserve12px compact owner text, with literal `Canvas DEMO`12px
header. Inner retains original480logical layout, scale1,16px body/12px small text.
Actual owner17regression tests and adapter4tests/strictClippy pass (owner receipt),
including128pixel ratio1/font>=12/geometry inside128 for list and details plus
selection/details navigation. Soil38tests/check/strictClippy/packagefmt pass;
static ARM64 artifact verified and host CLI produces49152-byte frame/startupready.
No physical readability claim. Parent reports both DRM modesetting paths now
start, but physical panel/text acceptance still belongs to parent/user.

## Superseded bounded inner/outer artifact

`target/soil-canvas-surfaces` is static stripped ARM64 SHA256
`00e4e43285ebc3cab611ed7ec01e6fb2806f7e9cbd89f057a5f6b5c564aa3645`.
Same embedded/exported Wasm guest hash as above. Reuses owner adapter's actual
Inbox+Coherence+egui CPU renderer for both; no resized copy of an inner bitmap,
no fallback UI. **Superseded for outer legibility:** this artifact's owner internally uses
logical480px layout scaled for the physical extent, reducing16px text to4.27px.
User reports USB preview text illegible even at4x. Scaling cannot restore lost
source glyph detail; do not treat this artifact as outer-readability acceptance.
A native128px responsive owner layout is being validated before its replacement.
Inner has scale1 and native16px body/12px small text; the same shrink path does
not explain inner scanout/preview problems.

Parent can run two independent bounded producers (separate owner state):

```sh
/run/soil-canvas-surfaces --guest /run/canvas-client.wasm --output /run/canvas.bgr --frames 6000 --interval-ms 100 --keypad-socket /run/fungos-device-provider --allow-keypad --surface inner
/run/soil-canvas-surfaces --guest /run/canvas-client.wasm --output /run/canvas-outer.bgr --frames 6000 --interval-ms 100 --surface outer
```

`--surface` must be last. Outer has no input grant in this example; it does not
mirror inner state or automatically switch on lid events. Parent chooses actual
outer presentation route and lifecycle; Soil adds no DRM/device owner. Wait for
`startup=ready` and verify921600/49152bytes respectively before presentation.
No unqualified outer card/node/format command is invented here.

38 workspace tests pass, including actual guest owner navigation at both extents,
opaque/nonuniform RGBA, exact BGR lengths, permission isolation, wrong-granted-
extent rejection, and typed HAL socket/restart-to-guest-to-owner. Workspace check,
strict all-targets Clippy, package fmt, and static crossbuild passed. Adapter owner
also reports its direct128x128 model selection/details tests pass. Parent's latest
`.089` inner remains physically garbled and outer unqualified: producer correctness
is **not** proof of panel scanout, channel order, stride, or physical readability.

## Keypad integration

HAL worker owns `fungos-device-hal::keypad_transport::KeypadClient` on existing
combined provider socket. No new evdev reader. Soil normalizes the current
qualified DT codes: arrows103/108/105/106, center28, Back158 (DT row4,col0
`0x0400009e`; physical acceptance still parent). Raw digits/media/power ignored.
Press dispatches once, release only clears held, directional repeat requires
held press; resync or error clears all held state with no guessed releases or
history replay. Provider owns ring bounds, cursor, freshness and reconnect.
Explicit guest permission is checked before client contact. Initial visible
artifact is deliberately available independently of keypad rollout.

Keypad-enabled artifact: `target/soil-canvas-keypad` static ARM64 SHA256
`c21e4ce8f0fc7d2540da025615f186b4d84da3cd9c6f79705cf93e400d098d82`.
The original no-input binary hash is `8dec10beca7571c8c6571ac223a2e3afd6f71a7b7a0f67dd985179ba1b599b22`.
Guest `target/canvas-client.wasm` hash
`16ada19d1d7a8ce5aae72c5995dfe725667619c69da2b9c1f25b89faf2291344`.

Parent replaces only the producer, retaining the presenter and last good frame:

```sh
/run/soil-canvas-keypad --guest /run/canvas-client.wasm --output /run/canvas.bgr --frames 6000 --interval-ms 100 --keypad-socket /run/fungos-device-provider --allow-keypad
```

Use HAL provider `ab6985191dd33eec1f2b1fa4d3540599325da861475e2991bc3368b4d1a28367`
or newer preserving the typed contract. `--deny-keypad` with socket specified
returns Denied without contact; omitting both flags disables input entirely.
Render/publish follows each normalized event before processing the next one;
logs identify host normalization -> guest input -> Canvas owner. Transport loss
is logged, doesn't kill display rendering, clears held state and retries via
HAL-owned reconnect/head semantics. Display remains last valid frame on producer
termination. Parent still owns producer/presenter lifecycle and physical proof.

Host test `tests/canvas_keypad.rs` exercises actual typed HAL ring/codec/socket,
Soil client and real Wasm imports into actual Canvas navigation/render, including
provider restart with no replay. It does not substitute for physical acceptance.

