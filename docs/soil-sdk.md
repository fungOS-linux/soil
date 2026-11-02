# Soil SDK

Status: **first guest feature, 2026-11-02.** `soil-sdk` is the umbrella crate
for guest-side capability helpers. The first feature is `audio`.

Naming: `soil-sdk` is the library. “Fertilizer” can be the separate scaffold /
project generator later; it should not be the import name.

## Shape

```toml
soil-sdk = { path = "...", features = ["audio"] }
```

```rust
use soil_sdk::audio;

fn run() -> u32 {
    match audio::play_tone(48_000, 2, 440.0, 200, 0.2) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
```

The umbrella does **not** generate one giant WIT world. It re-exports
capability crates, each with its own narrow guest world:

```text
soil-sdk
  └─ soil-sdk-audio
       └─ wasi:audio/playback
```

That keeps the component’s imports equal to the features the Spore actually
uses.

## `soil-sdk-audio`

- `Output::open(sample_rate, channels)`
- `write(&[f32]) -> accepted frames`
- `write_all(&[f32])` with host-scheduled drain on backpressure
- `drain(timeout_ms)`
- `status()`
- `close()`
- `tone(...)` and `play_tone(...)`

The guest owns sources, decoding and DSP. The host owns the device, format
conversion, final mix and clock.

## Proof

The audio tone fixture now uses the SDK:

```sh
cargo build --manifest-path crates/wasi-audio-wasmtime/fixtures/tone/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/wasi-audio-wasmtime/fixtures/tone/target/wasm32-wasip2/release/wasi_audio_tone.wasm \
  crates/wasi-audio-wasmtime/fixtures/tone.wasm

cargo run -p wasi-audio-wasmtime -- crates/wasi-audio-wasmtime/fixtures/tone.wasm
# audio: ok
```

## Next features

- `canvas` — normalized input and host-owned surface calls
- `ble` — typed scan/connect/read/write/notify helpers
- `storage` — CID reads/writes
- `time` — granted monotonic tick
- `webgpu` — optional wgpu-shaped helper

Each feature should have a real consumer before it lands.
