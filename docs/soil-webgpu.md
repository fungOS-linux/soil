# Soil WebGPU

Status: **host + Canvas presentation proof, 2026-11-02.** Soil runs real
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

- [`soil:canvas-gpu@0.1.0`](../crates/soil-webgpu/wit/deps/soil-canvas-gpu/package.wit)

It accepts a `wasi:webgpu` `gpu-texture` by borrow and returns nothing but a
success/error. It cannot create a device, texture, window, swapchain, or input
event. The host chooses the target extent through `size()`; the guest does not
control the Canvas surface.

## Host

[`crates/soil-webgpu`](../crates/soil-webgpu) is the host:

- a forked `wasi-webgpu-wasmtime` (`fungOS-linux/wasi-gfx-runtime` revision
  `9624cae`) binds the WIT and adds host texture readback;
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

This proves compute. It does **not** prove `wgpu::Surface`: the upstream WIT
has a `gpu-canvas-context` resource but no constructor, and a guest-owned
surface would bypass Canvas ownership.

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
soil::canvas_gpu::surface::present(&texture)?;
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
the window, target extent, final composition and presentation timing.

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

The wgpu fixtures need a GPU adapter and are tested with:

```sh
cargo test -p soil-webgpu --test wgpu_compute -- --ignored --nocapture
cargo test -p soil-webgpu --test wgpu_canvas -- --ignored --nocapture
```

Run the Canvas fixture and write the same BGR24 frame format used by
`soil-canvas`:

```sh
cargo run -p soil-webgpu -- --frame /tmp/triangle.bgr --size 480x640 \
  crates/soil-webgpu/fixtures/wgpu-canvas.wasm
# webgpu: ok frame=/tmp/triangle.bgr width=480 height=640 format=bgr24
```

The test asserts that the centre of the host-selected 480x640 frame is red,
the corners are the clear colour, and the frame is opaque.

## Limits of this proof

- Presentation is a full-frame handoff, not a composited Canvas layer yet. The
  owner UI does not blend a GPU texture with widgets; the next step is a
  `software-image`/layer adapter inside the Canvas owner.
- `gpu-canvas-context` remains unimplemented. It has no constructor in
  `wasi:webgpu@0.3.0-rc.2`; the narrow `soil:canvas-gpu` package is the
  presentation boundary instead of a guest-owned surface.
- `wasi-webgpu-wasmtime` still has TODOs and `unimplemented!` paths outside the
  compute/render/readback path.
- No per-device memory/quota policy beyond `wgpu-core` limits yet. That belongs
  in Soil manifest/grant enforcement before untrusted Spores use it.
