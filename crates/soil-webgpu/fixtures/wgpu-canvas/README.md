# wgpu 29 Canvas fixture

`wgpu-canvas.wasm` is a real wgpu 29 render pass built for `wasm32-wasip2`.
It creates a standard `wasi:webgpu` texture, clears it, draws a red triangle,
then hands the texture to `soil:canvas-gpu/surface.present`.

Regenerate:

```sh
cargo build --manifest-path crates/soil-webgpu/fixtures/wgpu-canvas/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/soil-webgpu/fixtures/wgpu-canvas/target/wasm32-wasip2/release/soil_wgpu_canvas.wasm \
  crates/soil-webgpu/fixtures/wgpu-canvas.wasm
```

The guest backend is pinned to `fungOS-linux/wgpu-wasi-webgpu` revision
`88938bb`; the host readback bridge is pinned to
`fungOS-linux/wasi-gfx-runtime` revision `9624cae`.
