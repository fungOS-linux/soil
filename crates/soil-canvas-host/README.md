# soil-canvas-host

Host half of [`soil:canvas@1.0.0`](../../wit/soil-canvas.wit). A component Spore
presents a versioned `soil-scene` document; this crate validates it against the
same ceilings and hands it to a `Presenter`.

```rust
use soil_canvas_host::{CanvasHost, CanvasRuntime, CanvasSocket};

let socket = CanvasSocket::connect("/path/to/canvas/ui.sock", "dashboard", "Dashboard", 1200, 840)?;
let host = CanvasHost::new(socket, 1200, 840, 2.0, theme);
CanvasRuntime::new()?.run(component_bytes, host)?;
```

`CanvasSocket` is the default presenter. It forwards the validated document to
Canvas's producer socket, so Canvas keeps ownership of the renderer. A host
without Canvas implements `Presenter` with its own renderer; the capability,
budget, generation and failure rules stay the same.

## Budgets

- One `present` and one `input` per invocation; call `CanvasHost::reset` before
  the next guest call.
- Documents are capped by `soil_scene::MAX_PAYLOAD_BYTES`, nodes
  `MAX_NODES`, and text `MAX_TEXT_BYTES`.
- A present after `close` is `closed`. A stale input generation is `stale`.

## Canvas wire

```text
{"kind":"hello","surface_id":"dashboard","title":"Dashboard","width":1200,"height":840}
{"kind":"present","payload":{"kind":"scene","schema":1,"nodes":[...]}}
{"kind":"close"}
```

Canvas replies with `state`/`error` lines and may interleave producer input
(`pointer`, `scroll`, `key`); the presenter retains a bounded input queue.
