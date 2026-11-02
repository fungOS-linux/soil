# Soil WebGPU

Status: **host proof, 2026-11-02.** Soil runs a real component that imports
upstream `wasi:webgpu@0.3.0-rc.2`, requests a GPU device, compiles WGSL, runs a
compute pass and reads the result back.

## Contract

The contract is **not** a Soil package. It is the upstream
[`wasi:webgpu`](https://github.com/WebAssembly/wasi-webgpu) WIT:

- [`wit/deps/wasi-webgpu-0.3.0-rc.2/package.wit`](../crates/soil-webgpu/wit/deps/wasi-webgpu-0.3.0-rc.2/package.wit)
- pinned by `crates/soil-webgpu` and asserted by a unit test.

The guest imports `wasi:webgpu/imports@0.3.0-rc.2`. There is no `soil:gpu`
draw-list contract and no second GPU ACL. Linking the package is the grant;
without it the component cannot instantiate.

## Host

[`crates/soil-webgpu`](../crates/soil-webgpu) is the host:

- `wasi-webgpu-wasmtime` 0.5.0 binds the WIT;
- `wgpu-core` 29 (Metal / Vulkan / GLES / DX12) owns adapter, device, shaders,
  pipelines, buffers, textures, queues and error scopes;
- one `Store` and `ResourceTable` per component, so two guests never share
  handles;
- no native device, surface, pointer, queue or mapped memory crosses into Wasm.

`wasi:io/poll` is linked because component-model async uses it as transport.
The `WasiCtx` has no preopens, environment, arguments or network policy; a guest
that imports a WASI authority it was not granted still fails to link.

## Proof

The fixture is a real component built from
[`fixtures/hello-compute`](../crates/soil-webgpu/fixtures/hello-compute):

```sh
cargo build --manifest-path crates/soil-webgpu/fixtures/hello-compute/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/soil-webgpu/fixtures/hello-compute/target/wasm32-wasip2/release/soil_webgpu_hello_compute.wasm \
  crates/soil-webgpu/fixtures/hello-compute.wasm
```

Run the GPU-gated test:

```sh
cargo test -p soil-webgpu --test hello_compute -- --ignored --nocapture
```

The guest returns `0` only when the Collatz compute results are `[0, 1, 7, 2]`.
Run any component with the CLI:

```sh
cargo run -p soil-webgpu -- crates/soil-webgpu/fixtures/hello-compute.wasm
```

## Limits of this proof

- No presentation yet. `wasi:webgpu` can render offscreen; a future
  `wasi-gfx:surface`/Canvas target decides how a Spore's pixels reach the wall.
  Canvas remains the owner of windows, layout and final composition.
- `wasi-webgpu-wasmtime` 0.5.0 still has TODOs and `unimplemented!` paths. The
  compute path is proven; texture/render/surface paths need conformance work.
- The existing `wasi-gfx/wgpu` guest backend is a wgpu 0.19 proof against an
  older WIT. Upstream `wgpu` 27 apps still need a current backend port before
  “source-compatible wgpu” is true.
- No per-device memory/quota policy beyond `wgpu-core` limits yet. That belongs
  in Soil manifest/grant enforcement before untrusted Spores use it.
