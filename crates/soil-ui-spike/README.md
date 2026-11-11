# soil-ui-spike

A deliberately small host for Spore UIs: taffy for boxes, parley for text, vello
for drawing, wgpu for the device, PUC Lua 5.4 for the view. It
renders offscreen to `out.png` so the stack can be judged without a window or a
browser.

This crate stands alone in the workspace on purpose: its own lockfile, its own
engine choice, nothing shared with `soil-lua` yet. Moving the engine question
into the SDK is a workspace-wide commit, not something one spike should force.

    cargo run                     # default pointer: none
    PX=40 PY=130 cargo run        # hover whatever is under (40,130)
    PX=359 PY=284 PBTN=1 cargo run  # hover and press a button
    SOIL_TRACE=1 cargo run        # print the draw commands the view emitted

## The engine: PUC Lua 5.4, on both hosts

The native host (`cargo run`) runs the view on PUC Lua 5.4 through `mlua` with
`vendored`: the Lua C sources compile with the crate instead of linking a system
library. The browser host (`browser/`) runs the *same* C sources on
`wasm32-wasip1` (`browser/lua.wasm`, built from the `lua-wasi` fork). One
implementation, two builds, so there is no dialect to reconcile.

This replaced an earlier [piccolo](https://crates.io/crates/piccolo) native
host. Piccolo is a pure-Rust Lua 5.4 and it drove the same untouched `view.lua`,
but its gaps were a contract we would have had to keep: no `string.format`, no
pattern family, no `table.concat/insert/remove/sort`, no `tonumber`. Maintaining
that slice across three hosts is a worse trade than running the interpreter the
sandbox and the wasm build already carry.

The consequence is blunt: on native, `Lua::new()` opens the whole safe standard
library, including `io` and `os`, and `require` reads the filesystem. That is
fine for a spike running code you wrote. It is not the spore sandbox — a spore
gets a bundle and no filesystem, which is what the browser host already does.

The failure mode we care about is a missing function showing up as a blank
frame. It does not: `view_paints_under_lua54` loads the real `theme.lua` and
`view.lua` and asserts the frame produced text and hit regions, and `Ui::new`
refuses a file that returns no table.

### Loading files

`view.lua` opens with `package.path = "./?.lua;" .. package.path` and then uses
`dofile` and `require` as Lua always does. Native resolves both against the
crate directory. On wasm, `browser/prelude.lua` and `browser/driver.lua` replace
`dofile`, `require` and `print` with lookups in the bundle `browser/build.mjs`
emits, so the view sources are byte-identical on both hosts and `package.path`
is inert there.

### The parity gate
`parity/native.trace` is the command stream the native host emits under
`PX=120 PY=100 PBTN=2`; `node browser/verify.mjs` asserts the wasm host emits the
same count and the same split, and `cargo test` in `crates/spore-guest` asserts
the guest library does too (that crate has its own workspace, so `-p` from the
root will not select it):

    rect 8 · text 9 · group 12 · hit 5   — 34 commands, three hosts

Pixels agree to renderer noise: 1–4 of 230,400 channels differ by one step
between any two runs of the *same* binary (vello's area antialiasing is not
bit-exact), so images are compared with

    cargo run --bin png_diff -- out.png other.png

not `cmp`. Same commands plus a picture inside that noise band is the result the
gate asked for.

## The primitives

Everything visual is data. Six host primitives, and only where the host is the
only thing that can know the answer:

| primitive | why host-side |
|---|---|
| `rect(x,y,w,h,radius,colour)` | GPU |
| `text(value,x,y,size,colour,anchor,align,font[,wrap])` | shaping; `anchor`/`align` need metrics |
| `hit(id,x,y,w,h,action)` | regions travel with the frame |
| `clip(x,y,w,h)` / `clip()` | vello layers |
| `measure(value,size,font)` | only shaping knows width |
| `load_font(name,path)` | font bytes to face, at runtime |

## The files

- `theme.lua` — colours, radii, spacing scale, type scale, font path.
- `ui.lua` — the styling layer: `box()` returns a padded content rect, `scope()`
  prefixes hit ids per component instance, `id()` reads the current prefix.
- `components/*.lua` — components are files: `render(ui, props)`, no registry.
- `view.lua` — composition only; no colours, sizes or offsets.

## What works

- Layout split by taffy; text shaped and positioned by parley with vertical
  (`top`/`baseline`/`middle`) and horizontal (`left`/`center`/`right`) anchors.
- Hit regions recorded per frame; the host hit-tests the *pointer against the
  last frame* and passes `hover`/`pressed` back in — the same loop a real host
  runs, minus the event source.
- Components with scoped ids, so two instances can share a label.
- `ui.scope(key, paint)` emits `group` markers, so every command carries the
  identity of the component that produced it — the hook a fragment cache hangs
  on. `ui.list(rect, count, row_height, scroll, paint_row)` emits only the rows
  that fit the extent plus one screen of overscan and clips them, which is why
  84 commands cover both 1,000 and 3,000 rows (see [Frame budget](#frame-budget)).
- A theme swap changes palette, radii, type scale and font with no rebuild.

## What is missing

Tracked in Marbles, in the order they will bite:

- SOIL-ct4u — input normalization, capture and drag in the host *(landed)*
- SOIL-ej3y — hover, focus, press, drag and modifiers carried into the frame *(landed)*
- SOIL-dze2 — focus scopes and accelerator routing
- SOIL-sbv2 — cursor shape and accessibility node per hit region
- SOIL-ryp2 — keyed lists for stable identity *(keys landed: `ui.scope` emits
  `group` markers; nothing consumes them yet)*
- SOIL-2gqd — scroll and clip containers as components *(landed as `ui.list`
  plus wheel scrolling into `state.scroll`; `ui.clip` was already there)*

Unlogged but known: a fragment cache behind the `group` markers (the scene is
still rebuilt whole when a frame changes), and nothing here is committed on this
machine.

Also absent and known: multi-line wrapping is not exercised, there is no event
loop or window, and nothing here is on the wasm path yet.

## Two front ends

    cargo run --window     # winit: a real window, real pointer events
    cargo run              # headless: pointer from the environment, PNG out

Both share one `Ui`; the router is the same in each, so a click behaves the same
whether it came from a window or from `PBTN=2`.

## Environment notes

- **A Delta worktree that synced to another machine may have no `.git`.** This
  checkout did not, so edits are on disk and sync, but commits have to happen
  where the repository actually is. Worth knowing before wondering why `git
  status` fails in a worktree that clearly has files.
- **A wedged `sccache` breaks any from-scratch build.** If `rustc` is invoked
  through `sccache` and dies, it is configured in `~/.cargo/config.toml`, so
  unsetting `RUSTC_WRAPPER` is not enough; pass
  `--config 'build.rustc-wrapper=""'`. Incremental builds hide this.
- **vello is not bit-exact between runs.** Two runs of the same binary differ by
a handful of channels by one step. Compare images with `png_diff`, and treat
the draw-command trace as the real contract.
- **`wgpu` must match the version `vello` uses** (`0.4` → `wgpu 23`). Two wgpu
  versions in one graph produce confusing "expected `vello::wgpu::Device`,
  found `wgpu::Device`" errors, because they are different crates.
- **vello needs a non-srgb surface format.** On macOS the surface offers
  `Bgra8UnormSrgb` first; asking vello to render into it fails with
  `UnsupportedSurfaceFormat`. Pick `Rgba8Unorm` or `Bgra8Unorm` explicitly.
- **`parley` must be built without its default `system` feature** on macOS here:
  enumerating system fonts crashes inside `objc2`'s Swift bridge. We load every
  face from a blob, so nothing is lost.

## Input state, in one table

The host passes one `state` table per frame: `hover`, `pressed`, `focus`,
`pointer {x,y}`, `mods {cmd,shift,alt,ctrl}` and `drag {x,y}` when a drag is in
flight. Views style from it and never track the pointer, the keyboard or the
modifiers themselves.

Focus follows a press into the control, and `Router::focus_next` walks the
regions in paint order for Tab / shift-Tab. The view draws the ring — there is
no stroke primitive yet, so a focused row shows an accent edge and accent text.

Enter or Space activates whatever has focus, through the same action path a
click takes; Escape clears it. The view registers an action next to each hit
region and the host decides what it means, which is why a click never mutates
view state directly: `open:<name>` from a nav row is what changes selection.

## Scale

Both hosts own the scale, and neither tells the view about pixels. The view is
laid out in logical pixels; the renderer applies the zoom to the finished
scene, so text is scaled as vectors and stays crisp.

- Native: base zoom from the window width (`width / 1000`, clamped 1–2×), times
a user preference adjusted with Cmd/Ctrl `+` and `-`.
- Browser: the same width-derived zoom, applied as a canvas transform.

A 1400-point window therefore runs the same 1000-point layout at 1.4×, instead
of a small UI floating in a big window.

## Wrapping

`text(..., wrap = width)` breaks lines at that width, and `measure(value, opts)`
returns `width, height` using *the same* wrapping, so a block can size itself
before it paints. `components/paragraph.lua` is the thin wrapper.

One trap worth knowing: parley only lays out glyphs for faces **registered with
its own collection**. With the `system` feature off, a face you draw with but
never registered lays out zero glyphs — a blank screen with no error. `load`
now registers every blob and remembers its family name for the font stack.

## Browser host (C Lua 5.4 in wasm)

`browser/` runs the same `view.lua` on **C Lua 5.4.7** compiled to
`wasm32-wasip1`, then paints the commands it emits with Canvas2D. No framework,
no bundler: one wasm module, one generated Lua text, one page. The page is the
host: it owns extent, pointer, hover/press/focus, selection and scale, exactly
as `window.rs` does natively.

The engine is built by the `lua-wasi` fork (Lua compiled as C++, so `pcall`
uses wasm exceptions instead of `setjmp`, which wasi-libc refuses). Two artefacts
come out of it: `lua.wasm` (a command module, for `test.lua`) and `spore.wasm`
(a **reactor**: `_initialize`, `lua_spore_buffer`, `lua_spore_run`). The page
loads the reactor and calls it once per frame with a few hundred bytes of Lua,
so the loader and the view modules are set up once, not per frame.

Its README has the one flag that matters — clang defaults to the legacy
exception encoding while wasi-sdk's prebuilt `libc++abi`/`libunwind` use the new
one, so both sides need `-mllvm -wasm-use-legacy-eh=false` or V8 refuses the
module.

    # in lua-wasi
    make                        # lua.wasm and spore.wasm, ~877 KB each
    cp *.wasm <this crate>/     # gitignored

    # here
    node browser/build.mjs       # bundle.lua + both wasm files beside the page
    node browser/verify.mjs      # headless: same shim, same module, same bundle
    (cd browser && python3 -m http.server 8765)   # open http://localhost:8765/

`verify.mjs` is the gate, and it runs without a browser:

- runs the module under a hand-written `wasi_snapshot_preview1` (19 imports: stdio
  captured, clock real, every path call refused — a spore brings its data),
- asserts the view emitted **34 commands** — the same count and kind split
  (8 rect, 9 text, 12 group, 5 hit) as `parity/native.trace`,
- runs the page's own painter against a stub context and counts draw calls, so a
  browser cannot show a half-painted frame from a throw in `paint.js`.

What is deliberately a stand-in in this path: `measure` is an estimate (the
native host asks parley), wrapping happens twice (once approximately in Lua,
for sizing; once for real in the canvas), sources are bundled into one chunk
instead of read from disk, and there is no input yet — the state is fixed.
Those are the seams where the real spore ABI (scene document in, WIT imports
out) replaces the recorder.

## Frame budget

`--bench` measures a view with N rows and splits the frame by stage. It loads
`bench_view.lua` (same recorder, same host) and reads its knobs from the
environment, so nothing about the bench leaks into the real view.

    SOIL_VIEW=bench_view.lua SOIL_ROWS=3000 SOIL_ITERS=20 cargo run --release -- --bench

| knob | meaning |
|---|---|
| `SOIL_ROWS` | rows the bench view emits |
| `SOIL_VIRTUAL` | emit only what fits the extent (plus one screen) |
| `SOIL_MUTATE` | change the hovered row every frame — the honest case, where a cache must miss |
| `SOIL_ITERS` | frames to time (default 30) |

Stages: `paint` (taffy + state tables + Lua), `scene` (shaping + scene build),
`shape` (of which shaping; shaped vs cached per frame).

Release numbers on this machine (`cargo run --release`), 1000×700, one change per
frame where marked, 60 frames:

| rows | virtual | mutate | commands | paint | scene | shaped/frame |
|---|---|---|---|---|---|---|
| 3,000 | no | no | 9,001 | 11.7 ms | 0 ms | 0 (3,000 cached) |
| 3,000 | no | yes | 9,001 | 12.2 ms | 12.3 ms | 0 (3,000 cached) |
| 1,000 | no | yes | 3,001 | 6.0 ms | 4.9 ms | 0 (1,000 cached) |
| 3,000 | yes | yes | 84 | 0.13 ms | 0.09 ms | 0 (27 cached) |
| 1,000 | yes | yes | 84 | 0.12 ms | 0.08 ms | 0 (27 cached) |

Debug builds are 4-18× slower on the same view (3,000 rows non-virtualised: 27 ms
paint, 226 ms scene when a row changes); the shape of the table does not change,
which is the point — virtualization is what moves the number, not the optimizer.

What that says: the shaping cache removes the dominant cost (it was 1,974 ms at
3,000 rows), the whole-frame scene cache makes an unchanged frame free, and
`ui.list` flattens everything — 84 commands whether the list holds 1,000 rows or
3,000. A single change in a *non-virtualized* list still rebuilds the whole
scene; that is the case the group markers and a fragment cache exist to fix.

`ui.scope` now emits `group` markers into the command stream, so every command
carries the identity of the component that produced it. Nothing consumes them
yet — they are the hook a fragment cache hangs on, and the reason a click no
longer has to be the only thing with an id.
