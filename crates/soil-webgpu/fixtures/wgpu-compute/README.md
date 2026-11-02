# wgpu 29 compute fixture

`wgpu-compute.wasm` is a real wgpu 29 program built for `wasm32-wasip2`.
It calls `wgpu_wasi_webgpu::instance()` and then ordinary wgpu APIs for a
Collatz compute pass. The component imports `wasi:webgpu@0.3.0-rc.2`; Soil's
`wasi-webgpu-wasmtime` host answers it.

Regenerate:

```sh
cargo build --manifest-path crates/soil-webgpu/fixtures/wgpu-compute/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/soil-webgpu/fixtures/wgpu-compute/target/wasm32-wasip2/release/soil_wgpu_compute.wasm \
  crates/soil-webgpu/fixtures/wgpu-compute.wasm
```

The guest backend is pinned to
`fungOS-linux/wgpu-wasi-webgpu` revision `88938bb`.
