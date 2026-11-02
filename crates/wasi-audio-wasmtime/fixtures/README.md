# `wasi:audio` tone fixture

`tone.wasm` is a real component built from `tone/` against the draft
`wasi:audio@0.1.0` PCM sink. It uses `soil-sdk`’s `audio` feature. It opens 48 kHz stereo, writes 200 ms of 440 Hz
sine, drains, and closes.

Regenerate:

```sh
cargo build --manifest-path crates/wasi-audio-wasmtime/fixtures/tone/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/wasi-audio-wasmtime/fixtures/tone/target/wasm32-wasip2/release/wasi_audio_tone.wasm \
  crates/wasi-audio-wasmtime/fixtures/tone.wasm
```
