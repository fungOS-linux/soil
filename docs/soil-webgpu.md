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

## wgpu 29 drop-in

A real wgpu 29 program now builds for `wasm32-wasip2` and runs in Soil:

```rust
let instance = wgpu_wasi_webgpu::instance();
let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))?;
// ordinary wgpu from here
```

The guest backend is a fork of [`wgpu-wasi-webgpu`](https://github.com/fungOS-linux/wgpu-wasi-webgpu)
updated from `wasi:webgpu@0.0.1` to `0.3.0-rc.2`. It uses wgpu's `custom`
dispatch traits, so wgpu itself is not forked for the backend. The fixture is
pinned to revision `88938bb`.

This proves compute. It does **not** prove wgpu `Surface`: `wasi:webgpu` still
needs a Canvas-owned presentation target, and `gpu-canvas-context` remains a
host TODO.

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
cargo run -p soil-webgpu -- crates/soil-webgpu/fixtures/wgpu-compute.wasm
# both print: webgpu: ok
```

The wgpu fixture needs a GPU adapter and is tested with:

```sh
cargo test -p soil-webgpu --test wgpu_compute -- --ignored --nocapture
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
