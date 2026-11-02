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
