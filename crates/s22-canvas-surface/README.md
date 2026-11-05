# Actual Canvas owner surface adapter

`reversal-ozgo`: first genuine Canvas Work Inbox render, CPU backend. This is **not** the separate synthetic GFX render/readback proof.

The parent explicitly permits reusing the implemented egui owner in `dock/.worktrees/soil-webgpu` despite older dock AGENTS requiring iced. Rationale: reuse actual existing owner model, tested narrow layout, keyboard handling and Coherence styling rather than reimplement that UI in a different toolkit. Desktop iced remains untouched. This crate adds no schema, widget renderer or rasterizer implementation.

## Ownership and contract

* Actual owner: `canvas-workspace-ui` from `fpl/dock/.worktrees/soil-webgpu/crates/canvas-workspace-ui`.
* `CanvasSurface::new(480,640)` obtains actual `demo::work()` Snapshot, calls its validation, embeds real `Inbox::show` with owner `Options{keyboard_active:true}`. Top provenance strip explicitly says **DEMO DATA**. Demo values do not claim live observations.
* `with_snapshot(width,height,Snapshot)` accepts validated host-owned actual owner data; provenance strip remains conservative DEMO, so do not advertise this as a live integration.
* Appearance: existing matching `coherence-native`, `Tokens`, compact/dark egui style. No copied palette.
* Raster: third-party `egui_software_backend =0.0.2` with default features disabled/std only. This version uses egui0.33, matching owner exact0.33.3. Version0.0.3 uses incompatible0.34 and is not selected. It receives egui-tessellated textured meshes/font atlas, not fpl_gfx::Scene. No native window system or GPU requirement.
* Rust crate `s22_canvas_surface`; concrete `CanvasSurface::{new,present,key}`, Frame `{width,height,rgba,generation}`, `Frame::bgr24()`. Key Up/Down/Left/Right/Enter/Back/Tab maps into genuine egui events consumed by Inbox owner. Enter/Right opens owner detail view, Back/Left closes, Up/Down owner selection. OpenSource external intents are not authorized or executed by this adapter.
* 480x640 is full-scale logical/physical output for current presenter. Other widths above160 scale the existing480px layout; widths<=160 use native logical pixels (scale1) and the actual owner's responsive compact branch. `CanvasSurface::new(128,128)` shows the real selected item title (bounded2lines), owner status and position/navigation hint; details mode shows real item owner. All text is12px, including literal `Canvas DEMO` provenance. Filtering/reconciliation/navigation/details actions run once in existing Inbox code before this branch. No fake outer UI, duplicated action logic or fallback. Output support and source OCR do not establish physical readability. Pending input bounded64 events, mesh jobs bounded32768; unsupported paint callbacks fail loudly. Fonts and demo dataset embedded locally, no network/filesystem content loading.
* Opaque output is backed by black/alpha255 initial target. Test verifies all output alpha255. CPU rasterization and CPU simpledrm presentation must be described honestly; no acceleration or native DSI claim.

Soil owner `66:pi-soil-ui-resume` consumes this via its injected `CanvasSurface` capability trait and actual Wasm guest, owns permission checks/imports, output publication, typed HAL key access. This crate does not duplicate that authority layer or publish a device file. Parent alone builds/stages on ARM64, deploys and observes phone. No raw guest framebuffer/guest widget schema.

## Reproduction and staging

```
cargo check --manifest-path cat-s22/tools/canvas-surface/Cargo.toml --locked
cargo test --manifest-path cat-s22/tools/canvas-surface/Cargo.toml --locked
cargo clippy --manifest-path cat-s22/tools/canvas-surface/Cargo.toml --locked --all-targets -- -D warnings
cargo run --manifest-path cat-s22/tools/canvas-surface/Cargo.toml --locked --example capture -- NEW_CAPTURE.ppm
```

Cargo.lock pins dependencies. Path dependencies are relative to current isolated reversal worktree and preserve exact existing owner realpaths. A staged build must preserve that sibling-worktree layout or explicitly change paths; never substitute an unrelated owner version silently. No parent owner sources modified.

Local check/test/strict all-target lint passed (4 tests). Native compact test asserts scale1/logical128/contextPPP1, ownerfont>=12 and all text row rectangles within128x128 in both list and details modes. Shared owner assertions run at both480x640 and128x128, verifying exact dimensions, nonuniform actual pixels, every alpha255, Down changes owner selected ID AND rendered pixels, Enter/Back alter actual owner details_open, exact BGR byte count. Separate bounds test rejects heights below128 and oversized extents. `Send` compile assertion also passes, though Soil may use a local trait. Capture example actually produced `/tmp/s22-canvas-owner-capture.ppm` and converted PNG; image inspection unavailable to this model, so no visual-legibility claim. Phone physical visibility, runtime frame timing, typed HAL navigation and Soil integrated guest qualification are parent/Soil follow-through, not established by this adapter test.

License boundary: actual owner crate declares `LicenseRef-Proprietary`; retain owner restrictions/notices. Third-party CPU backend MIT OR Apache2.0. This is scoped internal integration, not permission to publish owner source.
