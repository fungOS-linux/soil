# soil-brain

The Elmers dashboard's **brain** — every workflow, its links, and the function
that owns it — as a `soil_scene` document instead of a d3-force layout in a DOM.

```sh
curl -s http://127.0.0.1:8792/graph > graph.json
soil-brain --graph graph.json --out brain.json
soil-viz --scene brain.json
```

## Why a converter and not a renderer

A scene is the interface the GPU path already speaks: `soil-viz` plays one,
`soil-scene-gpu` rasterizes one, and a host may present one. Emitting the scene
keeps the drawing code in one place and lets the brain appear anywhere a scene
can — a window, a Canvas surface, or a PNG — without a second implementation.

## Layout is deterministic

Workflows are grouped into one arc per owning function, in the order the graph
lists them, at three radii. No simulation: a force layout lands differently on
every run, and a picture whose nodes move when nothing changed is a picture
nobody can point at. Position is a function of the graph, not of a clock.

| Mark | Means |
|---|---|
| amber rect | spends a model |
| red rect | gated (an `approve` on the path) |
| grey rect | neither |
| thin line | one workflow `run()`s another |
| small amber dot | a function hub |

Labels are dropped for long names once the graph is dense; at 300+ workflows a
wall of 9 px text is noise, and the shapes carry the structure.

## Checking it

```sh
cargo test -p soil-brain
```

The unit tests assert the scene validates (`Scene::validate`) and that two runs
produce byte-identical JSON.
