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
```

When the component host replaces the S22 core runner, these imports should move
to normal WIT linker bindings and this document should shrink to a compatibility
note.
