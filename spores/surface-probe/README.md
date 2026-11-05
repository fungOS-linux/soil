# surface-probe

Fixture Spore for `soil:surface-import@0.1.0`.

It exercises:

- opening a host-owned imported surface
- resize and frame/damage metadata
- normalized pointer and text input
- close

The host supplies a `SurfaceSource` adapter for the source id `fixture`. The
Spore never receives a native window handle, WGPU texture, or per-frame pixels.

```sh
cargo build --manifest-path spores/surface-probe/Cargo.toml \
  --target wasm32-wasip2 --release
```

The checked-in `module.wasm` is the test fixture.
