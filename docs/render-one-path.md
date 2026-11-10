# One renderer, four hosts

Status: **inventory + entry point, 2026-11-09.** The single path every host now
goes through, what it requires of a producer, and the two pieces still missing
before the browser joins it.

## The path

```text
Spore package
  │  [engine].target = "soil" (Lua) or a wasm module
  ▼
soil-lua            runs the view in the Canvas sandbox, resolves ink, and
                    emits a Present::Scene            ← one producer contract
  ▼
soil-scene-gpu core wgpu + fontdue, no pty, no window, no std::fs font
  ├─ native:  render() → Frame            (blocking readback, native only)
  ├─ Canvas:  GpuLease over the socket    (compositor imports the texture)
  └─ browser: draw_to(&surface view)      (no readback at all)
```

`soil-scene-gpu`'s render core is the one renderer. It is `wasm32-unknown-unknown`
clean (`--lib --no-default-features`), it acquires its device with
`Renderer::new_async()` because a browser hands one out through a promise, and it
takes its target as a `TextureView` so a canvas surface and an offscreen texture
are the same code path.

## Call it

```sh
scripts/render-spore.sh <package> <region|-> <width> <height> <out.rgba> [snapshot.json]
```

That is the whole pipeline in one command: package → document → pixels. The
browser viewer will differ only in where the pixels go.

## What a producer must get right

Two rules, both learned the hard way:

1. **Coordinates are normalized**: eight units span the granted extent on both
   axes, `translation` is the top-left corner in a y-up space, `scale` is the
   size in those units. A document in pixels renders one quadrant of its
   surface, because the renderer clips to the extent it was told.
2. **Text size is a ratio**, carried in `transform.scale`; the renderer
   multiplies it by its own body size. A document is pixel-accurate only against
   a host whose body size and face match the ones it was authored against.

## What is still missing

- **Canvas ingress by name.** Producers default to `…/Dock/canvas/ui.sock`;
  Canvas binds `…/Dock/canvas/external-surfaces.sock`. One path, two names.
- **The browser half.** `canvas-web-host` still serves PNG frames rendered by
  native Canvas. It needs a `/scene` endpoint, and `canvas-web` needs the viewer
  that calls `new_async` once and `draw_to` per frame. WebGPU first; the PNG
  mirror stays behind `?mode=mirror` and Canvas2D covers a missing
  `navigator.gpu`.
- **A presenter in soil.** Nothing calls `soil-lua` from a live host yet, so
  "runs in soil" is still two commands a person runs.
