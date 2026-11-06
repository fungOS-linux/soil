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
