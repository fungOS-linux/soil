# S22 bounded guest-frame proof (reversal-7lzz / reversal-7lzz.2)

This is a **synthetic geometric fixture**, not Canvas, JARVIS, a signed Spore,
or a replacement phone UI. It proves import-free Wasmtime execution, validated
scene transfer, actual owner GFX CPU rendering and normalized input. No GPU
rendering or physical phone/keypad qualification is claimed by host tests.
Phone deployment is owned by the parent; this worktree never accesses the phone.

## Sources and ownership

- `crates/soil-runtime/src/lib.rs`: persistent `GuestScene`, strict scene adapter.
- `crates/soil-runtime/src/evdev.rs`: host-only 64-bit Linux input adapter.
- `crates/soil-runtime/src/bin/soil-frame.rs`: explicit host file/device CLI.
- `crates/soil-runtime/fixtures/phone-shell.wat`: synthetic fixture; compiled to
  binary Wasm by `build.rs`, embedded in the host binary.
- `crates/viewer-spore/src/lib.rs` and `wit/soil-view.wit`: existing extracted
  Canvas viewer navigation owner and proposed capability contract. `Key::viewer_key`
  calls its actual `RemoteKey` mapping; up/down remain host chrome, left/right
  previous/next, OK select, Back follows its fullscreen/back semantics. The
  synthetic guest's four-row selection is deliberately **not** that contract.
  Real Canvas shell/surface integration is still pending.
- GFX dependency uses the existing owner-relative path convention, pointing to
  `../../../../../../fpl/gfx/.worktrees/s22-software` from the runtime crate;
  owner revision `82b14ff0ba5363b65609ca1a1616a387d186046b`. No vendored renderer,
  absolute dependency, GPU features, or duplicate rasterization.

Device authority, services and scanout remain host-owned and frontend-neutral;
ohmOS/Slint stays independent. No new service or policy implementation.

## Contract and bounds

Core Wasm only, no imports/WASI; 2 MiB module, 16 MiB per memory, one memory,
one table/4096 elements, one instance, fuel per start/run/input invocation.
Module compilation itself has no wall-clock deadline: only bounded input size.
`run: () -> i32` returns the scene offset; exported `scene_len: i32` gives byte
length. Optional `input: (i32) -> ()` receives normalized keys (up=1, down=2,
left=3, right=4, select=5, back=6); it is required only for interactive guests.

At most 4096 records, 32 bytes each, little endian:
`x,y,w,h: f32; kind,fill,outline,accent: u8; radius,value,rotation: f32`.
Only flat rounded rectangles (kind 0) and discs (kind 1), finite in-bounds
480x640 rectangles, enum roles 0..15. Outline must be role 13, value/rotation
zero. Disc radius zero and accent equals fill: unsupported fields cannot be
silently discarded. Rectangle radius <= half minimum dimension. GFX clears
uncovered pixels transparent; the synthetic fixture explicitly paints an opaque
background. GFX samples pixel centers. Raw output is RGBA8 (1,228,800 bytes,
1920-byte rows) or BGR24 (921,600 bytes, 1440-byte rows). No header/padding.
Frame file replacement is atomic via same-directory rename; a presenter must
reopen the path per update (not hold the old inode).

Evdev opens only the explicitly supplied path, read-only/nonblocking, no grab or
ioctl/write. Linux 64-bit native-endian 24-byte `input_event`, no guest pointers.
Only EV_KEY arrows 103/108/105/106, Enter/OK 28/352, Esc/Back 1/158. Other keys
(including power/media/digits) ignored. Fresh press dispatches once; release
clears held state without action; repeat dispatches only held directional keys.
Duplicate presses and invalid values ignored. SYN_DROPPED clears state and
suppresses through SYN_REPORT, then requires fresh press (no guessed resync).
Each invocation caps raw events at 4096 and timeout at 60s; partial records are
buffered with fixed storage, EOF/disconnect/truncation fails explicitly.
No time-based repeat throttle: bounded by event/time budgets, using kernel repeats.
This does not certify the actual device keymap; parent must inspect event codes.

## Exact host gates

From this worktree root:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p soil-runtime --bin soil-frame -- demo target/phone-proof.rgba rgba
printf 'down\nok\nback\nquit\n' | cargo run -p soil-runtime --bin soil-frame -- demo target/phone-next.bgr bgr24 --stdin
wc -c target/phone-proof.rgba target/phone-next.bgr
```

The tests cover exact GFX disc pixels/stride/transparent background, guest
bounds/schema/fuel/import/memory rejection, deterministic repeated frames,
persistent clamped fixture navigation, evdev press/release/repeat/drop and
actual Viewer Spore owner mapping. They do not prove physical input or Canvas UI.

## Cross-compile and parent-only phone proof

```sh
CARGO_BUILD_JOBS=2 cargo zigbuild -p soil-runtime --bins --release --target aarch64-unknown-linux-musl
file target/aarch64-unknown-linux-musl/release/soil-frame
shasum -a 256 target/aarch64-unknown-linux-musl/release/soil-frame
```

Binary: `target/aarch64-unknown-linux-musl/release/soil-frame`.
Parent stages to RAM only; no deployment command is run from this worktree.
On the phone, parent first runs the **noninteractive** command below and verifies
exit status, byte count/hash and presentation using its existing DRM presenter.
That establishes phone JIT/frame proof before interactive work:

```sh
/tmp/soil-frame demo /tmp/soil-proof.rgba rgba
wc -c /tmp/soil-proof.rgba
sha256sum /tmp/soil-proof.rgba
```

Only after that succeeds, parent can run a bounded input proof with its qualified
`event0` and arrange presenter file reloads:

```sh
/tmp/soil-frame demo /tmp/soil-proof.rgba rgba --evdev /dev/input/event0 4096 30000
```

This is a synthetic interactive proof, **not Canvas acceptance**. Full Canvas
render/navigation owner integration and phone visual evidence remain separate.
Keypad qualification precedes GPU investigation. Simpledrm/card1 scanout does
not prove Adreno rendering. No native panel/modesetting or guessed MMIO here.
