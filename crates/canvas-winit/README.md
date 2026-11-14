# canvas-winit

The smallest native presenter for a GPUI Spore: winit owns the window, the
Spore owns the pixels. This is the native end of the same boundary Canvas and
the future web bridge implement.

```sh
canvas-winit <module.wasm>
canvas-winit <module.wasm> --capture frame.rgba --frames 2 --key a --size 480x800
canvas-winit <module.wasm> --hue-bridge http://192.168.10.15 \
  --geode-socket ~/.geode/geode.sock --geode-purpose hue.control
```

`--capture` writes each `soil:present` frame as tightly packed RGBA; `--frames`
stops after N frames. `--key`, `--pointer`, `--click`, and `--drag` queue input
before the component starts, so a capture can prove the input path without a
human at the keyboard. `--size` sets the logical window extent, which is what
lets one module be checked at phone, flip, and desktop layouts.

## What it does

- Runs the component in a `wasmtime` store through `soil-webgpu`, so the Spore
  sees only `wasi:webgpu` and `soil:present`.
- Receives each frame through the `CanvasGpuPresenter` seam and blits it into a
  `softbuffer` window.
- Normalizes winit keyboard events into the `soil:present/input` queue the
  Spore polls. The host does not interpret them; the Spore does.
- Keeps the guest independent of the presenter: the same module runs under
  `soil-webgpu --frame`, `canvas-winit`, or (later) the HTTP/SSE bridge.

## Grants

A Spore gets only the capabilities the presenter was configured to provide:

- `soil:present` surface, input, cursor, and clipboard;
- `soil:spore/catalog` and launch, rooted at `--spores`;
- `soil:terminal-screen` backed by a real PTY;
- `soil:http` against `--allow-origin` origins plus the local storefront origin;
- `soil:records` for the read-only `vitals.view` namespace when `--records-root`
is set.

`--hue-bridge URL` binds the stable `http://hue.local` capability to a bridge.
The guest names that capability and a Geode handle; the host rewrites the
origin and substitutes the leased key into the Hue v1 path. A missing bridge or
credential returns `unavailable`/`denied`, never an invented success.

## Status

One window per process. Keyboard, pointer, scroll, cursor, catalog/launcher,
the PTY-backed terminal, and the macOS bundle are live. The one-shot
`soil-webgpu` host remains the capture/debug path.
