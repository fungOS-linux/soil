# Core-Wasm ABI profile

Some Soil capabilities are described as WIT for review and portability, but the
S22 runtime links them through a bounded **core-Wasm** lowering rather than the
component canonical ABI.

```text
soil:device/battery@0.1.0
soil:device/estimate@0.1.0
soil:device/lid@0.1.0
soil:canvas/surface@0.1.0
soil:canvas/animation@0.1.0   (runtime-only tick import)
```

This is intentional for the current S22 host: it runs core Wasm modules and
embeds the semantic contracts in Rust. It is not a second public ABI.

## Rules

- The WIT files remain the reviewed semantic contract.
- The core-Wasm import strings and layouts are host-private implementation.
- A component host must expose the same semantics through its own WIT binding.
- `soil:canvas/animation` is runtime-only and is not a Spore capability.
- No guest receives a new authority because the lowering is core-Wasm.

## Where it lives

```text
wit/soil-device.wit            semantic battery, estimate and lid
wit/soil-canvas.wit            semantic owner surface
crates/soil-runtime/src/       core-Wasm imports and layouts
crates/soil-runtime/fixtures/  core-Wasm fixtures using those imports
crates/soil-canvas-host/       component `soil:canvas@1.0.0` host
crates/soil-sdk-canvas/        component guest SDK
```

The component path is now real for new guests: `soil-canvas-host` links
`soil:canvas` and `soil:scene` through normal WIT bindings and validates the
same documents. The S22 core runner remains for the legacy `@0.1.0` owner
surface and its fixtures. When those fixtures move to the component host, this
document can shrink to a compatibility note.
