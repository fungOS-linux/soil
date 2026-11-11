# WebGPU fixture

`hello-compute.wasm` is a real component built from `hello-compute/` against
`wasi:webgpu@0.3.0-rc.2`. It requests an adapter/device, compiles WGSL, runs a
Collatz compute pass, maps the staging buffer, and returns `0` only when the
results are `[0, 1, 7, 2]`.

Regenerate:

```sh
cargo build --manifest-path crates/soil-webgpu/fixtures/hello-compute/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/soil-webgpu/fixtures/hello-compute/target/wasm32-wasip2/release/soil_webgpu_hello_compute.wasm \
  crates/soil-webgpu/fixtures/hello-compute.wasm
```

`wgpu-compute.wasm` is the real wgpu 30 drop-in fixture. It uses
`wgpu_wasi_webgpu::instance()` and the ordinary wgpu API, but imports
`wasi:webgpu@0.3.0-rc.2` and runs in Soil. See `wgpu-compute/README.md`.

`wgpu-test.wasm` adds the presentation proof: it draws with wgpu 30 and
calls `soil:present/surface.present` with a standard `wasi:webgpu`
texture. See `wgpu-test/README.md`.
