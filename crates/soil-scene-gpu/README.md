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

Rectangles are drawn by the GPU pipeline. Text is rasterized into a full-frame
alpha overlay by `fontdue` and composited by the GPU; a glyph atlas is the next
step. The resulting frame is sent as a Canvas `gpu_frame` lease, so Canvas keeps
the window, z-order and input.

The next transport is shared memory: keep the `gpu_lease`/revision metadata and
replace the base64 `rgba` field with a memfd ring.
