# Soil UI: the state of it

Status: **inventory, 2026-11-08.** What a Spore can draw, what draws it, where the
theme comes from, and what is still missing. Everything here was read out of the
trees named below. Nothing was compiled while writing it — a parallel build owned
the machine — so a claim that says "works" is a claim about code that exists and
is wired, not about a binary that ran today.

The canonical contracts are [`wit/soil-canvas.wit`](../wit/soil-canvas.wit),
[`wit/soil-scene.wit`](../wit/soil-scene.wit) and the decision records in
[`soil-canvas.md`](soil-canvas.md) and [`abi-map.md`](abi-map.md).

## 1. Four ways pixels reach a screen

| Path | Who produces | Payload | Who rasterizes | State |
|---|---|---|---|---|
| **Retained Lua module** | Lua author (`fungos.ui`) | Luau source + manifest + input snapshot | Canvas (mlua → iced) | works; the legacy path (§6) |
| **Scene document** | Spore or host | `soil_scene` v1 (17 node kinds) | `soil-scene-gpu` (wgpu + fontdue) | works, but see §7.2 — its ingress path and Canvas's disagree |
| **CPU frame** | Spore | `RgbaFrame` (tight RGBA8) | host blit | declared, implemented in `soil-scene` |
| **GPU lease** | Spore or producer | `GpuLease` (IOSurface / DMA-BUF / shm) | compositor | works; `soil-scene-gpu --hold` publishes one |

All four ride the same call: `soil:canvas@1.0.0/surface.present(document)`. The
WIT is explicit that `document` is a `soil-scene` payload and that the returned
`state` carries `generation`, host-chosen `width`/`height`/`scale`, `focused`,
`visible`, and the **resolved theme**. `soil:scene@1.0.0` exists only so a
producer can ask `supports(schema)` and `budget()` before it builds a document —
the document's shape is deliberately not a WIT type, so a new node kind never
rebuilds the Spores that draw.

`soil:surface@1` (a Spore submitting a UI tree of its own) is **deferred, not on
the v1 path**. `soil:view@1` (host-owned web/Spore/retained content inside
Canvas) **is** v1. That distinction is the load-bearing one in the whole design:
the scene payload rides on `soil:canvas.present`; it is not a second ABI.

## 2. The three surface shapes

**Composited.** On Linux, Canvas is the Wayland/KMS compositor
(`crates/canvas-linux`, built with `--features compositor`); it owns scanout,
z-order, input and the shell. On macOS and Windows it is a window manager with
the Accessibility work pushed into an independently installed helper
(`Dock Control.app`), because a normal GUI rebuild must not overwrite a
permission-bearing app. Chrome — the bar, launcher, switcher — is a **region
composited inside the shell**, declared by a Spore (`[chrome] regions =
["top-bar"]`), with a built-in implementation that the host keeps exactly like a
last-good scene.

**Windowed.** `soil-vt-window` (a real pty, `soil_vt::Host`, the same GPU
terminal renderer, a winit surface) and `soil-terminal` need no compositor at
all. This is the honest "native" answer today: a Spore can be a window without
Canvas.

**Web.** `canvas-web-host` serves `http://127.0.0.1:8088/?surface=<id>` and polls
the retained scene, so a `tend` reload appears without a browser refresh. That is
the fastest way to see a full dashboard, because a native surface may be placed
at the compositor's frame size while its viewer window is narrower.
`soil-search-host` serves `launcher.html` the same way. Neither is a second UI
contract: both render what the host already retained.

## 3. Three ways to write a UI

| You write | You get | You give up |
|---|---|---|
| **Lua** (`fungos.ui`, `fungos.chart`, `graph`, `map`, `viz`, `dashboard`, `atlas`, `live`) | fastest iteration: `tend check` catches it, `--scene` renders it headlessly, no Rust build | you run inside a host that embeds Lua — today only Canvas does (§7.1) |
| **Rust** (`soil-scene` structs, `fpl-gfx` primitives, `Canvas` widgets) | typed, testable, no interpreter | a rebuild to change anything, and the whole binary |
| **Native** (Wayland client, winit window, wasm guest) | full control of its own pixels and event loop | the host's theme, layout, input and composition, unless it takes them as grants |

The Lua column is worth being precise about. `soil-sdk-lua` **generates** a Lua
file from the WIT (`30 packages, 44 interfaces, 146 functions`), but it is names
and arity only: every function is one call to
`__soil.invoke(grant, interface, function, arguments)`. There is **no Lua engine
in the soil workspace at all** — no `mlua`, `luau` or `piccolo` in `Cargo.lock`.
The only Lua VM in the ecosystem is Canvas's (`mlua`, in `crates/canvas`). So
"WASM + Lua, running in soil" is currently "WASM in soil, Lua in Canvas".

## 4. What has been ported, and how far

**ChartGPU** is not a dependency anywhere. It is a *visual language* the Lua SDK
reproduces: `fungos.chart` is the chart layer, `examples/chartgpu/` is the
reference dashboard, and `docs/chart.md` states that exact pixel parity is not a
goal. The separate question "should Canvas adopt a charting crate" was answered
**adopt nothing** after a six-agent evaluation (`dock/docs/charting-dependency-decision.md`),
because every candidate failed on a different axis; the useful findings there
were zero `canvas::Cache` usage and `iced::Animation` already being in the tree.

**GraphGPU** — I could not find it. It is not in `soil`, `spores`, `dock` or
`gfx`, and there is no such directory under `~/src`. The nearest things are
three different things: `fungos.graph` (a composable graph layer over the scene
vocabulary, Lua), `soil:graph` (a *records* index — links with `from`/`rel`/`to`,
not visualization), and `gfx/topology.rs` (signal-flow geometry). Worth
confirming which one you mean before anything is claimed as ported.

**Ported into `fpl-gfx`** (the Rust half of the same vocabulary):

| Module | Owns |
|---|---|
| `plot` | bounded, theme-role chart geometry; series, axes, legends stay with the producer |
| `signals` | DAW and instrumentation views (transport, meters, notes) |
| `console` | read-only mixing strips, needle and LED meter faces, calibrated dB |
| `patchbay` | paired-jack annotations and declared wiring mode |
| `topology` | bounded signal-flow geometry |
| `attention` | five exclusive caller-observed attention buckets |
| `panels`, `data_views`, `media`, `code` | editorial panels, count mosaics, cover shelf, code/disassembly |
| `reactor` | assistant activity orb (visual only, not a lifecycle) |
| `mesh`, `skeletal`, `analysis` | shared mesh ABI, glTF CPU skinning, scalar fields bound to vertices |
| `geo`, `geo_map`, `globe`, `map_tiles` | WGS84 f64 positions, raster extents, globe, tiles |
| `retained_video`, `retained_waterfall`, `video_matte` | media frames, spectrum waterfalls, color keys |
| `chrome`, `symbols`, `orientation`, `orbit`, `motion`, `view_appearance` | shell metrics, shared outlines, camera/orientation, easing, appearance |
| `lovr` | a **frozen shader catalog with `ABI_VERSION`** a Lua host compiles itself |

Canvas does reference seventeen of these (`chrome` 27 times, `mesh` 14,
`reactor` 10, `geo_map` 8, `media`/`console` 7, `voice_popup`/`retained_video` 5,
…). See §7.5 before believing a local build of that.

## 5. Where the theme comes from

Not from Lua, and not from a package. There are four vocabularies and the host is
the source of truth for all of them:

| Layer | Vocabulary | Set by |
|---|---|---|
| Contract | `theme { mode, background, surface, text, muted, primary, success, warning, danger, info }` | the host, on every `state` observation |
| Canvas | `ProjectTheme` from Coherence; `module.theme` derived at invocation time | project identity (`coherence-native`, `ThemeFormat`) |
| Rust renderer | `fpl_gfx::Theme` — 16 `ThemeRole`s plus `ShapeTheme`, `MaterialTheme`, `MotionTheme`, serialized and versioned | the application |
| Lua | semantic ink names (`"background"`, `"accent"`, `"muted"`) resolved by `ui.theme()` / the surface | the host, passed in |

A Spore never ships a palette; it names inks. That is why four themes
(graphite/paper/amber/geode) needed no view change — but those files are not in
the trees today, so the theme *package* story is unproven: `[theme]` is read by
the registry, and no package in `spores/` declares one.

## 6. Against the legacy Canvas approach

Legacy Canvas owned the UI outright: an iced widget wall (`live.rs`, ~3.9k lines,
eleven `impl canvas::Program` blocks), hand-rolled OKLCH series colors, every
widget fully re-tessellating on every repaint because no `canvas::Cache` is used
anywhere, and a `canvas.*` Luau API producing Canvas's own scene type. Beauty was
capped by what that renderer could express — no materials, no shaders, no
device-level effects — and every host re-implemented its own drawing.

Now: the Spore owns composition, Canvas owns windows/input/z-order/theme, and the
payload is a versioned document. The gains are real — one scene vocabulary
across browser, native and composited hosts; a renderer that can grow (gfx has
sixteen material-bearing modules and a shader catalog); chrome as a region with a
fallback rather than as code in the shell.

The losses are just as real and should not be glossed: iced's text layout and
input routing have no equivalent yet in the scene path (`soil-scene-gpu`
rasterizes text with fontdue into an atlas, and *"input routing back into the pty
is still the host's job"*), and nothing in the scene path can do what Canvas's
retained modules do per-keystroke.

## 7. What to fix, in order

1. **A Lua host in soil.** Until `crates/soil-lua` exists, a Lua Spore runs on
exactly one host (Canvas, on macOS/Linux desktop) and the thesis is untrue
everywhere else — including the native `soil-vt-window` path and Android.
2. **One ingress path.** `soil-scene-gpu` and `soil-ui-demo` default to
   `…/Dock/canvas/ui.sock`. Canvas binds
   `…/Dock/canvas/external-surfaces.sock` (a Unix **datagram** socket,
   `canvas_core::external_surface_socket_path`, handled by
   `canvas/src/external_surface_binding.rs` → `canvas-model-viewer`). Same idea,
two names, no fallback: a producer that follows the soil README talks to nothing.
3. **Draw the shader node.** `soil_scene::Content::Shader { wgsl, uniforms }` is
declared and validated with the doc comment *"a new shader is data, not a
build"* — and no renderer has an arm for it. `soil-scene-gpu` skips it silently.
That single missing arm is the whole dynamic-shader feature; gfx's `lovr`
catalog is the promotion mechanism to copy (`ABI_VERSION` + frozen names).
4. **One theme vocabulary.** Six colors in the contract, sixteen roles in the
renderer, a third set of ink names in Lua. Derive two of them; do not maintain
three lists by hand.
5. **Repair the gfx pin.** Canvas's `[patch]` points `../gfx` at
`~/src/gfx`, which is 78 commits behind the `83b7d46` Canvas pins, and is
missing `src/mesh.rs` and `src/console.rs` that Canvas's source already calls.
A local build either fails or silently builds a different library than the one
claimed.
6. **`tend`'s node ceiling is now the tighter one.** The host default went from
256 to 8 192 nodes; `tend` still preflights at `--max-nodes 256` and 64 KiB. The
tool should not be the binding constraint on a view the host would accept.
7. **Two `fungos.ui`s.** `spores/sdk/ui/ui.lua` (811 lines, node/prims tree) and
   `fungos-sdk/lua/fungos/ui.lua` (1 379 lines, the `surface:` API every current
view actually calls). One package, one version, or every Spore pins a different
library under the same name.
8. **Two bars.** `spores/divebar` claims `top-bar` and ships a composed artifact;
   `spores/chrome` claims the same region in a manifest that currently declares
   `[module] path = "module.wasm"` for a file that does not exist. Pick one.
9. **Pointer → node.** `soil:canvas.input` is generation-bound and semantic, but
nothing maps a pointer hit back to a node id in a scene document. Canvas has
`interaction.rs`, gfx has `HitTarget`; the document has no field for it.

## 8. Building, testing and opening a Spore consistently

Appearance, no Rust in the loop:

```sh
lua tools/tend.lua check spores/divebar/ui/divebar.lua \
  spores/divebar/ui/snapshot.lua --width 1200 --height 44 --no-workspace
lua tools/tend.lua spores/divebar/ui/divebar.lua spores/divebar/ui/snapshot.lua \
  --name divebar --surface divebar --no-workspace        # writes + reloads
lua tools/tend.lua watch <view> <snapshot> --name N --surface N
lua tools/tend.lua --scene /tmp/bar.json <view> <snapshot> --width 1200 --height 44
```

`--scene` is the portable checkpoint: it writes a `soil_scene` document that
`soil-scene-gpu` can rasterize with no Canvas present.

Package gates (in this order; each refuses what the previous one could have
caught):

```sh
soil-spore lock   spores/divebar      # digest every tracked file
soil-spore check  spores/divebar      # manifest, deps, engine, abi
soil-spore pack   spores/divebar -o dist/divebar-0.1.0.spore   # prints CID
soil-spore verify spores/divebar      # lock against what is on disk
soil-spore unpack <archive> <dir>     # verifies, then writes
soil-spore cid    <archive>           # the address the bytes hash to
soil-spore digest spores/divebar      # what the lock will record
soil-spore sign   spores/divebar      # PGP sidecar
```

Present, by target:

| Target | Command |
|---|---|
| Composited, retained module | run `canvas`, then `tend … --surface S` |
| Composited, GPU scene | `soil-scene-gpu --scene X --canvas-socket <external-surfaces.sock> --source-id S --size WxH --hold` |
| Browser | `cargo run -p canvas-web --bin canvas-web-host --features native-host` → `http://127.0.0.1:8088/?surface=S` |
| Native window, no compositor | `soil-vt-window --shell`, or `soil-terminal` |
| Android | `soil-runtime` + `s22-canvas-surface` (egui 0.33.3, software backend) |

What is missing is the one command the phrase "open a Spore" implies: there is
no `soil-spore run <dir>`. Opening a package today is *lock → check → compose →
tend check → tend build or --scene → start a presenter*, across two
repositories and two environment conventions (`FUNGOS_*` for `tend`,
`SOIL_LAYOUT` for the layout fallback). A `spore open` that composed to a temp
file and handed it to `soil-scene-gpu`, falling back to `tend check` output,
would make the invariant checkable — and would make §7.2 impossible to ignore.

Package inventory today: 17 directories carry a `spore.toml`, 7 have a
`spore.lock`, and exactly one (`divebar`) ships a composed artifact in `dist/`.
