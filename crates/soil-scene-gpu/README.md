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
