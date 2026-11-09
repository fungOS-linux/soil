# soil-scene-gpu

Rasterize a `soil_scene` document into one RGBA frame with wgpu.

```sh
SOIL_LAYOUT=target/debug/soil-layout \\
  ~/src/fungOS/fungos-sdk/bin/tend --scene /tmp/calendar.json \\
  --width 1000 --height 900 \\
  ~/src/fungOS/fungos-sdk/examples/calendar/calendar.lua \\
  ~/src/fungOS/fungos-sdk/examples/calendar/live.lua

soil-scene-gpu --scene /tmp/calendar.json \\
  --canvas-socket "$CANVAS_CONFIG_DIR/ui.sock" \\
  --source-id calendar-gpu --size 1000x900 --hold
```

Rectangles are drawn by the GPU pipeline. Text glyphs are rasterized once into a
GPU glyph atlas by `fontdue`, then drawn as textured quads. The resulting frame
is sent as a Canvas `gpu_frame_shm` lease by default, so Canvas keeps the window,
z-order and input and no base64 image crosses the socket.

`--base64` keeps the older inline transport for debugging. The shared-frame path
is file-backed under `/tmp/fungos-frame-*`; a memfd/ring can replace it without
changing the lease message.

## Terminal

`soil-vt-gpu` turns a `soil_vt::Snapshot` into rect/text scene nodes and uses
the same GPU renderer:

```sh
soil-vt-gpu --text $'$ hello\r\n' --cols 80 --rows 24 \
  --canvas-socket "$CANVAS_CONFIG_DIR/ui.sock" --source-id vt-gpu --hold
```

Cell backgrounds become rects, runs of equal style become text nodes, and the
cursor is a rect. ANSI true/palette colours and selection/inverse are resolved
before the scene is built. `--shell` or `--command` runs a real pty, feeds `soil_vt::Host`, and republishes
a shared frame whenever the grid generation changes:

```sh
soil-vt-gpu --canvas-socket "$CANVAS_CONFIG_DIR/ui.sock" --source-id vt-gpu \
  --cols 80 --rows 24 --size 960x540 --command /bin/sh
```

Input routing back into the pty is still the host's job for Canvas surfaces;
the resident renderer already redraws on output instead of waiting for a
one-shot snapshot.

## Native terminal window

`soil-vt-window` is the standalone host: a real pty, `soil_vt::Host`, the same
GPU terminal renderer, and a winit surface. No Canvas or browser is involved.

```sh
cargo build -p soil-scene-gpu --bin soil-vt-window
./target/debug/soil-vt-window --shell
./target/debug/soil-vt-window --command /bin/zsh
```

Keyboard input goes back to the pty, including arrows, function keys, Ctrl
sequences and Alt prefixes. Resizing updates both the pty and the emulator grid.
`Ctrl+Shift+Q` quits the host. The window keeps one `Renderer`, so a new wgpu
device is not created per frame.

`soil-viz` and `soil-vt-window` both use `soil_scene_gpu::Renderer`; the free
`render` function remains a one-shot convenience for tests and tools.
## Live visualization

`soil-viz --live` takes a Lua-authored `LiveSpec` and runs the animation loop in
Rust. Lua owns layout, titles, palettes and styling; Rust owns resident data
sources, delta time and frame generation. There is no per-frame Lua or FFI
stream bridge.

```sh
cargo build --release -p soil-scene-gpu --bin soil-viz
./target/release/soil-viz --live /tmp/fungos-viz-live.json --fps 60
```

`soil-live-frame` renders one deterministic frame for screenshots and tests:

```sh
cargo run -p soil-scene-gpu --bin soil-live-frame --   --spec /tmp/fungos-viz-live.json --time 3.2 --rgba /tmp/live.rgba
```

The host reuses one wgpu device, renderer, target texture and bind group. It
waits until the next `Instant` deadline, advances by the measured delta, and
reloads the spec (not the process) when the file changes.
## Direct window rendering

`soil_scene_gpu::window::WindowRenderer` creates the scene pipelines for the
surface format and draws straight into the swapchain. Native windows no longer
render offscreen, read the frame back to the CPU, upload it to another texture
and blit it. One `wgpu::Device`, one renderer, one target.

`soil-viz` and `soil-vt-window` both use this path. The terminal also loads a
monospace face, derives cell width from the font advance, and uses the
`Theme::terminal` palette so cell runs stay aligned and readable.
