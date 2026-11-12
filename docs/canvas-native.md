# Canvas native surfaces

Status: **integration plan.** How a Spore's pixels reach Canvas native (the
windowed compositor), and what is already there versus what we add.

## What Canvas already has

- **Contract** — `canvas_core::external_surface`: a producer sends
  `Register { surface }`, `Present { source_id, generation, slot, sync, … }`,
  `Withdraw { … }` over an owner-local Unix datagram with `SCM_RIGHTS`
  descriptors, and receives an `ExternalSurfaceAcceptance` receipt. Transports:
  `SharedMemory`, `DmaBuf`, `IoSurface`, `IoSurfaceMachPort`.
  (`canvas_core/src/external_surface.rs`, `external_surface_ipc.rs`.)
- **Ingress** — `crates/canvas/src/external_surface_binding.rs` owns the socket;
  `crates/canvas-model-viewer/src/external_surface.rs` imports the accepted
  generation **into the same wgpu device iced renders with**, so it composites
  as an ordinary texture. Unit tests cover slot mapping, generation ordering,
  input and acceptance.
- **Producer precedent** — `canvas-media-edge/src/bin/native-window.rs`:
  ScreenCaptureKit → `IOSurface` → register/present. Runbook:
  `docs/macos-native-window.md`; the source is attached with
  `canvasctl surface add-external <source> native/<source>`.
- **Not yet verified** — live capture *and GPU import* end to end (the runbook
  says so explicitly). That is the gap our test should close.

## The bridge from a Spore

```text
Spore (wasm32-wasip2, wgpu/GPUI)
  │  soil:present / wasi:webgpu texture
  ▼
soil host adapter (producer)
  │  SharedMemory (step 1)  or  IOSurface (step 2, macOS)
  ▼
canvas_core::external_surface  ──►  Canvas native (iced + wgpu) composites
```

Step 1 is portable and one copy: take the `CanvasFrame` that `soil-webgpu`
already produces (the `wgpu-test` fixture proves it), write it into a shared
mapping, `Register` once, `Present` per frame. Step 2 wraps the guest texture's
Metal allocation as `IoSurface` for zero-copy; it needs `wgpu` hal access, so it
is a follow-up, not a prerequisite.

Ownership stays as decided: Canvas owns the window, the device and composition;
the Soil host owns the guest and the producer; input flows back over the
surface's input transport and is delivered to the Spore as `soil:input`.

## Test plan

1. **Canvas import** — `cargo test -p canvas-model-viewer external_surface`
   (existing unit tests).
2. **Producer protocol** — a test-pattern producer (checkerboard, shared
   memory) registers and presents; assert the inbox exposes the presented slot
   with the right pixels.
3. **Spore frame** — replace the checkerboard with the `wgpu-test.wasm` frame
   from `soil-webgpu`; assert the red triangle centre pixel survives the trip.
4. **Windowed smoke** — run Canvas native, `canvasctl surface add-external`,
   record the visible surface. This is the “windowed but still a Spore” check,
   and it needs a display.

## Blockers in this worktree

- Canvas does not build here: the workspace has sibling path dependencies on
  `../gfx`, `../unibus/...` and `../coherence-tokens`, and only `dock` is
  attached (`fpl-gfx` manifest missing). Attach those to run steps 1–4.
- The installed `/Users/ajmwagar/.local/bin/canvas` predates the
  external-surface importer (Oct 4); do not test against it.

## The terminal is the counter-example

The terminal Spore owns VT parsing (guest `libghostty-vt`) and submits a
**bounded cell grid** (`soil:terminal`); the host owns fonts, shaping, pixels.
That is a semantic payload, not pixels, and it is the right split for a
grid-heavy app. `soil-vt` is the host-side variant (bytes → snapshot → scene)
for hosts that parse instead of the guest. Both remain valid: `soil:surface`
keeps scene/RGBA/texture as the three payloads, and a grid is a specialised
scene producer. Do not force terminals into guest-rendered textures.
