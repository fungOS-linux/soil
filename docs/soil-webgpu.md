# Soil WebGPU

Status: **host + Canvas presentation proof, 2026-11-02; superseded by the
[GPU-spore decision](gpu-spores.md).** The host moves to `wgpu` 30 and
`wgpu-core` retires with the wasmtime fork; this document stays as the proof of
the WIT, the texture handoff and the readback path. Soil runs real
components that import upstream `wasi:webgpu@0.3.0-rc.2`, request a GPU device,
compile WGSL, run compute and render passes, read results back, and hand a
standard WebGPU texture to a host-owned Canvas frame.

## Contract

The contract is **not** a Soil package. It is the upstream
[`wasi:webgpu`](https://github.com/WebAssembly/wasi-webgpu) WIT:

- [`wit/deps/wasi-webgpu-0.3.0-rc.2/package.wit`](../crates/soil-webgpu/wit/deps/wasi-webgpu-0.3.0-rc.2/package.wit)
- pinned by `crates/soil-webgpu` and asserted by a unit test.

The guest imports `wasi:webgpu/imports@0.3.0-rc.2`. There is no `soil:gpu`
draw-list contract and no second GPU ACL. Linking the package is the grant;
without it the component cannot instantiate.

Canvas presentation is a separate, presentation-only package:

- [`soil:present@0.1.0`](../crates/soil-webgpu/wit/deps/soil-present/package.wit)

It accepts a `wasi:webgpu` `gpu-texture` by borrow and returns nothing but a
success/error. Its `input` interface carries normalized key, pointer, and scroll
events in one ordered stream, and `cursor` lets the Spore ask what it is pointing
at. The host chooses the target extent through `size()` and reports device scale
through `scale()`; the guest does not control the Canvas surface or open a
window.

Renderer Spores that need the Spore-owned view model also import
`soil:records`. The host links one read-only namespace, `vitals.view`, backed by
the `dev/personal/telemetry` book. It cannot write, scan another collection, or
name a filesystem path.

## Host

[`crates/soil-webgpu`](../crates/soil-webgpu) is the host:

- a forked `wasi-webgpu-wasmtime` (`mycelium-linux/wasi-gfx-runtime` revision
  `9624cae`) binds the WIT and adds host texture readback;
- `wgpu-core` 29 (Metal / Vulkan / GLES / DX12) owns adapter, device, shaders,
  pipelines, buffers, textures, queues and error scopes;
- `soil:records` is a read-only `vitals.view` namespace; the renderer cannot
  mutate the store or read health signals directly;
- `soil:http` is the bounded host transport from `soil-host`; origins,
  credentials, methods, body caps, and deadlines are host policy;
- one `Store` and `ResourceTable` per component, so two guests never share
  handles;
- no native device, surface, queue or mapped memory crosses into Wasm. Input is
  normalized and queued by the presenter; the guest owns hit-testing.

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

The guest backend is a fork of [`wgpu-wasi-webgpu`](https://github.com/mycelium-linux/wgpu-wasi-webgpu)
updated from `wasi:webgpu@0.0.1` to `0.3.0-rc.2`. It uses wgpu's `custom`
dispatch traits, so wgpu itself is not forked for the backend. The fixture is
pinned to revision `88938bb`.

This proves compute. It does **not** prove `wgpu::Surface`: the upstream WIT
has a `gpu-canvas-context` resource but no constructor, and a guest-owned
surface would bypass Canvas ownership.

## Native presenter

[`crates/canvas-winit`](../crates/canvas-winit) is the real desktop presenter:
winit owns the window, `soil-webgpu` owns the component, and `soil:present` is
the only surface contract. Pointer, key, scroll, cursor, catalog, PTY, HTTP, and
records grants are wired there. `--capture` uses the same host path for
screenshots, and `--size WxH` exercises the layouts a phone or flip presents.

## Canvas presentation

The render fixture uses ordinary wgpu 29 APIs, then crosses the texture as a
standard `wasi:webgpu` resource:

```rust
let texture = device.create_texture(&wgpu::TextureDescriptor {
    // RENDER_ATTACHMENT | COPY_SRC
    // ...
});
// draw with a normal wgpu render pass
queue.submit([encoder.finish()]);

let texture = wgpu_wasi_webgpu::host_texture(texture);
soil::present::surface::present(&texture)?;
```

The host:

1. validates the texture has `copy-src`;
2. copies it into a mapped readback buffer with
   `command_encoder_copy_texture_to_buffer`;
3. accepts only tightly packed RGBA8/BGRA8 formats;
4. gives the resulting `CanvasFrame { width, height, rgba }` to a
   `CanvasGpuPresenter`.

The default presenter discards the frame. The CLI and tests use `FrameCapture`,
which retains exactly one frame. No guest callback runs on the host; no native
window, swapchain, pointer, or mapped memory crosses into Wasm. Canvas still owns
the window, target extent, final presentation decision and timing.

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

A renderer that reads `vitals.view` names the records root explicitly:

```sh
cargo run -p soil-webgpu --bin soil-webgpu -- \
  --records-dir .soil-dev/records \
  --frame /tmp/vitals.bgr --size 1280x900 spores/vitals-view/module.wasm
```

The wgpu fixtures need a GPU adapter and are tested with:

```sh
cargo test -p soil-webgpu --test wgpu_compute -- --ignored --nocapture
cargo test -p soil-webgpu --test wgpu_test -- --ignored --nocapture
```

Run the Canvas fixture and write the same BGR24 frame format used by
`soil-canvas`:

```sh
cargo run -p soil-webgpu -- --frame /tmp/triangle.bgr --size 480x640 \
  crates/soil-webgpu/fixtures/wgpu-test.wasm
# webgpu: ok frame=/tmp/triangle.bgr width=480 height=640 format=bgr24
```

The test asserts that the centre of the host-selected 480x640 frame is red,
the corners are the clear colour, and the frame is opaque.

## Limits of this proof

- Presentation is a full-frame handoff, not a composited Canvas layer yet. The
  owner UI does not blend a GPU texture with widgets; the next step is a
  `software-image`/layer adapter inside the Canvas owner.
- The records grant is deliberately one read-only collection. A renderer that
  needs another view model adds a namespace here rather than receiving the
  whole store.
- `gpu-canvas-context` remains unimplemented. It has no constructor in
  `wasi:webgpu@0.3.0-rc.2`; the narrow `soil:present` package is the
  presentation boundary instead of a guest-owned surface.
- `wasi-webgpu-wasmtime` still has TODOs and `unimplemented!` paths outside the
  compute/render/readback path.
- No per-device memory/quota policy beyond `wgpu-core` limits yet. That belongs
  in Soil manifest/grant enforcement before untrusted Spores use it.
