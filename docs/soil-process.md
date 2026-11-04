# Soil threaded processes

Status: **cpal proof, 2026-11-03.** Soil runs an ordinary cpal program compiled
to `wasm32-wasip1-threads`. The cpal callback runs on a guest thread and pushes
PCM through a bounded host ring; the host device callback never enters Wasm.

## Why a second lane

`wasm32-wasip2` components are single-threaded and cooperative. They are the
right lane for capability-safe Spores, Canvas, WebGPU and `wasi:audio`.
Native-shaped libraries such as cpal need a real callback thread.

The process lane is a threaded core-Wasm module:

- `wasm32-wasip1-threads`
- shared linear memory
- `wasi.thread-spawn`
- WASI preview 1 syscalls
- Soil process imports (`soil_audio` today)
- the same host, quotas and device ownership model as the component lane

It is not a second capability system. It is an execution model for code that
cannot run cooperatively.

## cpal

The cpal fork adds a `wasm32-wasip1-threads` host backend:

- `cpal::default_host()` is the Soil host on this target;
- `build_output_stream` creates a guest-owned callback thread;
- `Stream::play()` starts that thread;
- the callback fills a fixed block;
- the guest thread writes interleaved f32 frames through:
  - `soil_audio_open(sample_rate, channels)`
  - `soil_audio_write(handle, pointer, frames)`
  - `soil_audio_close(handle)`

The host owns a cpal output stream and a bounded lock-free ring. Its audio
callback only drains the ring. It never calls Wasm, never borrows a guest
`Store`, and never holds a guest lock.

The cpal application source is unchanged. The only difference is the target and
the cpal fork.

## Run

```sh
cargo run -p soil-process -- crates/soil-process/fixtures/cpal-tone.wasm
# soil-cpal: ok frames-written=59392 frames-played=58880 underruns=0
```

The counts prove both directions: the guest callback produced PCM, and the host
device callback consumed it.

GPU-gated style device test:

```sh
cargo test -p soil-process --test cpal_tone -- --ignored --nocapture
```

## Current limits

- Only output streams; cpal input is not exposed yet.
- Fixed 512-frame callback block; no device-specific buffer negotiation.
- A fresh WASI context per guest thread; only the audio device and thread
  factory are shared.
- `soil_audio` is a process import, not a component WIT package. A future
  component/process adapter can route both lanes to one host mixer.
- Tokio is not implemented. The process lane is the prerequisite.
