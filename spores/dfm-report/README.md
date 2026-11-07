# dfm-report

DFM findings for a model, plus a native 3D view of the geometry each finding is
about.

Two surfaces, one dataset:

| Surface | What it is | Where it lives |
|---|---|---|
| `canvas` / `web` | the findings, as a Lua projection | `ui/report.lua` |
| `native` | the model, lit, with the flagged patch in amber and a marker per finding | `soil-dfm-viz` (crate `soil-dfm-viz`) |

Both read the same `model/data.json`, so the list and the 3D view can never
disagree about what is flagged. Nothing in this Spore re-runs a DFM analysis: the
report and the per-triangle patch assignment are inputs.

## The scene file

`model/data.json` is produced outside the Spore:

```sh
# 1. segment the mesh; this records per-triangle patch assignment
cargo run -q -p transmog-cli --release --bin transmog -- patches part.step

# 2. run DFM and keep the JSON contract
cargo run -q -p transmog-cli --release --bin transmog -- dfm part.step --json > /tmp/dfm.json

# 3. join them into the scene the Spore and the viewer both read
python3 scripts/build-dfm-spore.py patches.json /tmp/dfm.json model/data.json
```

A finding gets a 3D marker only when its rule maps onto recognised geometry.
Everything else is listed and marked `—` in the pin column: a finding is never
dropped and never given an invented anchor.

## Native view

```sh
cargo run -p soil-dfm-viz -- --scene spores/dfm-report/model/data.json
```

Drag to orbit, scroll to zoom, Esc to close. The marker under the cursor is the
active finding and its card follows it.

To check the renderer without a window (useful in CI, and how this was
verified):

```sh
cargo run -p soil-dfm-viz -- --scene model/data.json \
  --screenshot /tmp/dfm.bmp --size 1280x900
```

`--screenshot` writes 24-bit BMP with no image dependency.

## Build the module

```sh
cargo build --manifest-path spores/dfm-report/Cargo.toml \
  --target wasm32-wasip2 --release
cp spores/dfm-report/target/wasm32-wasip2/release/dfm_report_spore.wasm module.wasm
cargo run -q -p soil-spore --bin soil-spore -- lock spores/dfm-report
cargo run -q -p soil-spore --bin soil-spore -- check spores/dfm-report
```

`check` reports `ok dev.fungos.dfm 0.1.0 9 files <digest>`.

## What is deliberately absent

The module imports nothing. The Spore has no filesystem, no network, and no say
in the renderer: `soil:vfs` read access to the bundled scene is the only grant,
and the 3D surface is opened by intent (`dfm.open_native`), not drawn by the UI.
