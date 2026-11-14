# wgpu 30 compute fixture

`wgpu-compute.wasm` is a real wgpu 30 program built for `wasm32-wasip2`.
It calls `wgpu_wasi_webgpu::instance()` and then ordinary wgpu APIs for a
Collatz compute pass. The component imports `wasi:webgpu@0.3.0-rc.2`; Soil's
`wasi-webgpu-wasmtime` host answers it.

The guest backend is the `mycelium-linux/wgpu-wasi-webgpu` fork, branch `wgpu-30`
(rev `bec9213`), patched to wgpu 30. The host is still wgpu-core 29;
the WIT boundary is version-independent, and this fixture prints `webgpu: ok`
against it.

Regenerate:

```sh
cargo build --manifest-path crates/soil-webgpu/fixtures/wgpu-compute/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/soil-webgpu/fixtures/wgpu-compute/target/wasm32-wasip2/release/soil_wgpu_compute.wasm \
  crates/soil-webgpu/fixtures/wgpu-compute.wasm
```

The guest backend is pinned to
`mycelium-linux/wgpu-wasi-webgpu` revision `88938bb`.
