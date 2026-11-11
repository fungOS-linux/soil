# canvas-winit

The smallest native presenter for a GPUI Spore: winit owns the window, the
Spore owns the pixels. This is the native end of the same boundary Canvas and
the future web bridge implement.

```sh
canvas-winit <module.wasm>
canvas-winit <module.wasm> --capture frame.rgba --frames 2 --key a
```

`--capture` writes each `soil:present` frame as tightly packed RGBA; `--frames`
stops after N frames. `--key` queues one synthetic key before the component
starts, so a capture can prove the input path without a human at the keyboard.
It is the headless check used to compare presenters.

## What it does

- Runs the component in a `wasmtime` store through `soil-webgpu`, so the Spore
  sees only `wasi:webgpu` and `soil:present`.
- Receives each frame through the `CanvasGpuPresenter` seam and blits it into a
  `softbuffer` window.
- Normalizes winit keyboard events into the `soil:present/input` queue the
  Spore polls. The host does not interpret them; the Spore does.
- Keeps the guest independent of the presenter: the same module runs under
  `soil-webgpu --frame`, `canvas-winit`, or (later) the HTTP/SSE bridge.

## Status

One window per process, with a live keyboard loop. Pointer input, the
catalog/launcher, a long-lived PTY-backed terminal, and the macOS bundle are
next. The one-shot `soil-webgpu` host remains the capture/debug path.
