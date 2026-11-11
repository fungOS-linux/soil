# GPU-backed spores

Status: **decision.** Supersedes the assumption that the host owns a scene
renderer for spore UI. `soil-scene-gpu` is on its way out; GPUI (or iced, egui,
or raw wgpu) runs in the guest and writes pixels.

## Status (this thread)

- **A GPUI Spore runs as a native Mac app.** `canvas-winit` presents its frames,
  feeds it keys and pointer events, and owns the pty a terminal Spore renders.
  Contracts, startup path, flags, packaging and limits are in
  [`canvas-winit.md`](canvas-winit.md). `launcher-gpui` and `terminal-gpui` are
  the two Spores proving it; both are plain components with no native code.


- **Guest shim at wgpu 30: done and proven.** The patched fork lives at
  `fungOS-linux/wgpu-wasi-webgpu`, branch `wgpu-30`, rev `bec9213`. Both
  fixtures build for `wasm32-wasip2` and run against the existing wgpu-core 29
  host (`webgpu: ok`; the present fixture's 480×640 frame has a red centre and
  clear corners). The WIT boundary is version-independent.
- **Host still wgpu-core 29.** Retiring it means either a `wasi-gfx-runtime`
  bump or the `wgpu` 30 implementation of `wasi:webgpu` (item 2).
- **GPUI in a spore: compile frontier reached.** `gpui-fast` core and
  `gpui-fast-wgpu` both compile for `wasm32-wasip2` as shipped. The headless
  path that would drive them (`HeadlessAppContext` + `WgpuHeadlessRenderer`)
  is behind `test-support`, which does not build for wasm (pulls
  `proptest`/`wait-timeout`), and both renderer and context are gated
  `not(target_family = "wasm")`. Next fork: `gpui-fast` branch `wasip2` with
  (a) a wasm-safe headless feature, (b) `WgpuHeadlessRenderer` allowed on wasm,
  (c) an offscreen `WgpuContext` constructor over a caller-supplied `Instance`.
- **Names:** `soil:canvas-gpu` is now `soil:present`, and the fixture is
  `wgpu-test` (was `wgpu-canvas`).

## Roles

- **Canvas — compositor.** Owns the window, the event loop, the `wgpu` device
  and queue, final composition and frame pacing. It does not know which UI
  framework a Spore used.
- **Soil — wasm runtime.** Instantiates the Spore, mediates grants, and moves
  textures across `soil:surface` / `soil:present`.
- **Guest — the Spore.** Any wasm UI stack: GPUI, iced, egui, or raw wgpu. It
  owns its scene, renders into its own texture, and presents it.
- **Shim — small, in-guest.** The only framework-specific code we own: map the
  framework's surface/present hook to our WIT imports.

`soil-canvas-gpui` is not a thing. GPUI is not a Canvas component; it is guest
code. Canvas stays framework-agnostic.

## Device ownership (open item 2)

**The host owns one `wgpu` 30 device. `wgpu-core` is not the host API and goes
away with the wasmtime fork.**

Why: `wgpu` is the API every candidate framework uses (gpui-fast 30, egui-wgpu
30, iced 27), and a `wgpu::Texture` cannot cross into a `wgpu-core` `Global`.
The current host (`crates/soil-webgpu`) reaches `wgpu-core` 29 through the
`wasi-gfx-runtime` fork, which is exactly the mismatch. Implementing
`wasi:webgpu` over our own `wgpu` 30 device with a resource table keeps one
queue, one device-loss story and zero-copy handoff for every framework.

Guests are **version-independent**: they speak the `wasi:webgpu` WIT, so a
guest may be on wgpu 27 (iced today) while the host is on 30. Only the guest's
shim must match its own wgpu major. Today there is a shim for 29 only
(`wgpu-wasi-webgpu`), which is why "bump to 30" is a shim item, not just a
version bump.

## The three probes (open items 3-5)

Pass = the framework renders a frame on WebGPU (browser page) or through
`wasi:webgpu` (component) and hands us a texture, with no native window in the
path.

| probe | framework | wgpu | state today | first move |
|---|---|---|---|---|
| GPUI | `gpui-fast-web` | 30.0.1 | its wasm dependency is `default-features = false, features = ["webgl"]` — **WebGPU is not compiled**; `BROWSER_WEBGPU | GL` falls back to GL | enable wgpu's `webgpu` feature on wasm (patch/fork, then upstream) |
| Iced | `iced` 0.14 / `iced_wgpu` | 27 | default feature is `wgpu/default`, which includes `webgpu`; `webgl` is an optional fallback | build the web example and check the adapter |
| Custom | raw wgpu | 30 | nothing to integrate | smallest proof; also the reference for the shim |

Fourth data point, useful as a reference: `eframe` 0.37 defaults to `wgpu`
(30) rather than glow, so egui's web path is WebGPU-first already.

Note the asymmetry: on wasm, each framework decides which `wgpu` features to
compile. The host cannot fix that; the probe must turn the feature on in the
framework build.

### Two wasm worlds (this decides 3-5)

"As long as it's wasm" hides two different targets, and every framework is
currently ready for only one of them:

- **Browser page (`wasm32-unknown-unknown` + wasm-bindgen).** GPUI-web, iced
  and eframe all target this. WebGPU is available if the framework compiles
  `wgpu`'s `webgpu` feature; GPUI-web does not (it compiles `webgl` only),
  iced and eframe do.
- **Component (`wasm32-wasip2`).** This is what a Spore is: it imports
  `wasi:webgpu` and presents through `soil:surface`. None of the three is
  ready here today, and the blockers are mechanical but real:
  - `gpui-fast-wgpu` gates `web-sys`/`js-sys`/`wasm-bindgen` on
    `target_family = "wasm"`, which is also true for `wasip2`; those deps are
    browser-only and must be narrowed to `target_os = "unknown"` (or a
    `wasi` feature added).
  - Each framework needs an in-guest surface: for GPUI that is a `Platform`
    implementation over `soil:surface` (most methods can return
    `not-supported`), for iced a custom compositor surface, for raw wgpu just
    the WIT calls.
  - The guest needs a `wgpu` `custom` backend for its own wgpu major that
    speaks the WIT. Today the only such backend is the 29-era
    `wgpu-wasi-webgpu` fork.

So the probes split: the browser-page test answers "does this framework render
on WebGPU at all", and the component test answers "can a Spore ship this
framework". Do both, in that order.

## What retires, and when

- **`crates/soil-scene-gpu`** — as the renderer for Spore surfaces. Phase it:
  Lua views still need it until item 6 (a Lua-to-GPUI binding), and its GPU
  decimation is the only part with no GPUI equivalent — chart Spores will own
  their own wgpu compute once they render their own texture.
- **`wasi-gfx-runtime` / wgpu-core 29** — replaced by our `wgpu` 30 host.
- **Scene payload as the default** — `soil:surface@1.1.0` already carries three
  presentations (scene, RGBA frame, GPU-texture lease). The recommended payload
  becomes the **lease**; `scene` remains the compatibility path for guests
  without a device (Lua, until item 6). The WIT does not change.

## Consequences worth writing down

- **Charts move into the Spore.** `GpuDense`'s `reduction` (average / min-max /
  lttb) we just added only matters for the scene payload. A GPUI chart Spore
  renders its own series with its own wgpu compute and presents the texture.
- **Accessibility still comes from the guest.** A texture is opaque to
  AccessKit. Either the Spore publishes a parallel a11y tree (host injects it)
  or the framework does; GPUI's `Element::a11y_synthetic_children` is the
  in-framework version of that. This is a shim responsibility, not a Canvas
  one.
- **Input still comes from the host.** `soil:input` normalization and hit
  testing do not disappear; the shim translates host input events into the
  framework's event queue.

## Order of work

1. Host: `wgpu` 30 device owner; retire the `wgpu-core` path (items 1-2).
2. Shim: a `wgpu` 30 `custom` backend for the WIT, plus a guest `Platform`
   implementation for GPUI (item 3).
3. Probes: raw wgpu component first (the reference chain), then GPUI in a
   browser page with `webgpu` enabled, then GPUI as a component, then iced.
4. Lua-to-GPUI exposure; retire `soil-scene-gpu` when Lua no longer needs it
   (item 6).
