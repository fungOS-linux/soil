# Simple JARVIS opt-in

`CanvasSurface::simple_jarvis(width,height)` preserves `new()` as actual Canvas Inbox. Frame/key contracts unchanged. Keys in this static demo are no-ops, not fabricated actions. No guest pixels/schema are accepted.

Canonical GFX dependency remains `fpl/gfx/.worktrees/s22-software`, default features disabled. Its `Theme::jarvis_hud` capability is narrowly ported from `fpl/gfx/src/theme.rs`; all palette/shapes/materials and representable motion fields preserved. The older MotionTheme lacks `ease` and `quantum`, omitted explicitly rather than widening unrelated schemas. No palette is copied into the adapter. Existing dirty gfx lib/software/tests left untouched.

`simple_jarvis.rs` is a thin egui-painter projection of `reactor::visit(Activity::Resting,0.0)` Marks, with uniform fit and clipping into host bounds. Existing egui software backend rasterizes. No new widget renderer, wgpu dependency, telemetry, health, battery, microphone or audio assertion. Literal JARVIS DEMO and one Demo / Idle line. Native128 outer uses12px fonts; inner480 uses18px labels, no full HUD.

```
cargo run --manifest-path cat-s22/tools/canvas-surface/Cargo.toml --locked --example capture -- NEW.ppm 128 128 simple-jarvis
```

Local adapter check/tests5/strictalltargetClippy pass. Owner canonical palette validates via adapter test. Added gfx theme regression test validates primary and roundtrip, but direct gfx test invocation hit60s compiling its unrelated existing heavy dev-dependencies; not reported as passed. Actual captures `/private/tmp/s22-simple-jarvis-128.png` and `-480.png`; native128 OCR recognizes JARVIS DEMO and Demo / Idle. Image-capable review unavailable (classifier API rejects image input despite catalog), so physical/artistic/color qualification remains parent/user. Theme is not a fix for presenter byte ordering or outer pink gate.

Parent alone owns Soil CLI/host changes, crossbuild/deploy/phone. No Soil build executed here.

## Aurora FX (opt-in)

`CanvasSurface::set_aurora(true)` (or `soil-canvas --aurora` with
`--ui simple-jarvis`) paints a deterministic vertical aurora curtain behind the
reactor. It uses only the host-supplied animation time, 32 bounded vertical
quads, and a cyan/teal/violet palette; there is no random source, device read,
guest authority, or per-pixel work.

The default surface is unchanged. `--aurora` requires `--ui simple-jarvis`.
The adapter test `aurora_is_opt_in_and_animated` checks opt-in, time variation,
and that a surface without the flag is unchanged. `hue_wraps_at_the_primary_boundaries`
covers the palette wrap.
