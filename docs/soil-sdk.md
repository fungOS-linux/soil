# Soil SDK

Status: **first guest features, 2026-11-04.** `soil-sdk` is the umbrella crate
for guest-side capability helpers. Features are `audio`, `appdata`, and the
shared `soil-sdk-codec` trait.

Naming: `soil-sdk` is the library. “Fertilizer” can be the separate scaffold /
project generator later; it should not be the import name.

## Shape

```toml
soil-sdk = { path = "...", features = ["audio", "appdata"] }
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
  ├─ soil-sdk-audio
  │    └─ wasi:audio/playback
  ├─ soil-sdk-codec
  │    └─ serde JSON adapter
  └─ soil-sdk-appdata
       └─ soil:appdata/store
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

## `soil-sdk-appdata`

```rust
use soil_sdk::appdata::synced::Json;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Settings {
    theme: String,
}

let settings = Json::<Settings>::open("settings")?;
let mut value = settings.load_or_default()?;
value.theme = "dark".into();
settings.save(&value)?;
```

- `local::Json<T>` is device-only.
- `synced::Json<T>` is per-app, per-user, replicated and encrypted by the host.
- `load`, `load_or_default`, `load_with_meta`, `save`, `save_if_generation`,
  `delete`, `list_keys`, `sync_state`.
- `soil-sdk-codec` provides the `Codec` trait and `JsonCodec`.

App identity comes from the signed manifest. The Spore never supplies an app id
or a host path.

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

- `records` — typed/dynamic books, refs, and query helpers
- `canvas` — normalized input and host-owned surface calls
- `ble` — typed scan/connect/read/write/notify helpers
- `storage` — CID reads/writes
- `time` — granted monotonic tick
- `webgpu` — optional wgpu-shaped helper

Each feature should have a real consumer before it lands.
