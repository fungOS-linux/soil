# wgpu 30 Canvas fixture

`wgpu-test.wasm` is a real wgpu 30 render pass built for `wasm32-wasip2`.
It creates a standard `wasi:webgpu` texture, clears it, draws a red triangle,
then hands the texture to `soil:present/surface.present`.

Regenerate:

```sh
cargo build --manifest-path crates/soil-webgpu/fixtures/wgpu-test/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/soil-webgpu/fixtures/wgpu-test/target/wasm32-wasip2/release/soil_wgpu_test.wasm \
  crates/soil-webgpu/fixtures/wgpu-test.wasm
```

The guest backend is the `mycelium-linux/wgpu-wasi-webgpu` fork, branch `wgpu-30`
(rev `bec9213`), patched to wgpu 30; the host readback bridge is still
pinned to `mycelium-linux/wasi-gfx-runtime` revision `9624cae` (wgpu-core 29).
The WIT boundary is version-independent, so a 30 guest runs on the 29 host:
`webgpu: ok frame=… width=480 height=640 format=bgr24`.
