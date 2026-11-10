# soil-ui-spike

A deliberately small host for Spore UIs: taffy for boxes, parley for text, vello
for drawing, wgpu for the device, Lua for the view. It renders offscreen to
`out.png` so the stack can be judged without a window or a browser.

This crate is excluded from the workspace on purpose: it links `mlua` with
`lua54`, while `soil-lua` links `mlua` with `luau`, and only one crate in a graph
may `links = "lua"`. The engine decision is a workspace-wide commit, not a
per-crate one.

    cargo run                     # default pointer: none
    PX=40 PY=130 cargo run        # hover whatever is under (40,130)
    PX=359 PY=284 PBTN=1 cargo run  # hover and press a button

## The primitives

Everything visual is data. Six host primitives, and only where the host is the
only thing that can know the answer:

| primitive | why host-side |
|---|---|
| `rect(x,y,w,h,radius,colour)` | GPU |
| `text(value,x,y,size,colour,anchor,align,font)` | shaping; `anchor`/`align` need metrics |
| `hit(id,x,y,w,h,action)` | regions travel with the frame |
| `clip(x,y,w,h)` / `clip()` | vello layers |
| `measure(value,size,font)` | only shaping knows width |
| `load_font(name,path)` | font bytes to face, at runtime |

## The files

- `theme.lua` — colours, radii, spacing scale, type scale, font path.
- `ui.lua` — the styling layer: `box()` returns a padded content rect, `scope()`
  prefixes hit ids per component instance, `id()` reads the current prefix.
- `components/*.lua` — components are files: `render(ui, props)`, no registry.
- `view.lua` — composition only; no colours, sizes or offsets.

## What works

- Layout split by taffy; text shaped and positioned by parley with vertical
  (`top`/`baseline`/`middle`) and horizontal (`left`/`center`/`right`) anchors.
- Hit regions recorded per frame; the host hit-tests the *pointer against the
  last frame* and passes `hover`/`pressed` back in — the same loop a real host
  runs, minus the event source.
- Components with scoped ids, so two instances can share a label.
- A theme swap changes palette, radii, type scale and font with no rebuild.

## What is missing

Tracked in Marbles, in the order they will bite:

- SOIL-ct4u — input normalization, capture and drag in the host
- SOIL-ej3y — hover, focus, press, drag and modifiers carried into the frame
- SOIL-dze2 — focus scopes and accelerator routing
- SOIL-sbv2 — cursor shape and accessibility node per hit region
- SOIL-ryp2 — keyed lists for stable identity across frames
- SOIL-2gqd — scroll and clip containers as components

Also absent and known: multi-line wrapping is not exercised, there is no event
loop or window, and nothing here is on the wasm path yet.
