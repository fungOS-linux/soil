# canvas-winit

The smallest native presenter for a GPUI Spore: winit owns the window, the
Spore owns the pixels. This is the native end of the same boundary Canvas and
the future web bridge implement.

```sh
canvas-winit <module.wasm>
canvas-winit <module.wasm> --capture frame.rgba
```

`--capture` writes the first `soil:present` frame as tightly packed RGBA and
exits; it is the headless check used to compare presenters.

## What it does

- Runs the component in a `wasmtime` store through `soil-webgpu`, so the Spore
  sees only `wasi:webgpu` and `soil:present`.
- Receives each frame through the `CanvasGpuPresenter` seam and blits it into a
  `softbuffer` window.
- Keeps the guest independent of the presenter: the same module runs under
  `soil-webgpu --frame`, `canvas-winit`, or (later) the HTTP/SSE bridge.

## Status

One frame per run today. The component's `run` export returns after presenting;
the next step is a long-lived guest loop with `soil:surface` input, then the
catalog/launcher path (`spores/launcher-gpui`) and the macOS bundle.
