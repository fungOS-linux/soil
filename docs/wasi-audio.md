# Draft `wasi:audio`

Status: **host proof, 2026-11-02.** A real component imports draft
`wasi:audio@0.1.0`, opens a 48 kHz stereo sink, pushes 200 ms of 440 Hz PCM,
drains, and closes.

This is a Soil-maintained proposal, not an accepted WASI standard. There is no
upstream `wasi:audio` package today.

## Contract

[`wit/wasi-audio.wit`](../wit/wasi-audio.wit) is a bounded PCM sink:

- `open(format) -> output`
- `write(interleaved f32 frames) -> accepted frames`
- `drain(timeout-ms)`
- `status()` with queue depth, underruns and latency
- `close()`

The host owns the output device, sample conversion, final mix, volume and clock.
The guest owns sources, decoding and DSP. No cpal stream, native device handle,
or callback crosses the boundary.

`write` returns `0` for backpressure. It is not an error. A guest should retry
or shorten its block. `drain` is bounded to 2 s.

## Host

[`crates/wasi-audio-wasmtime`](../crates/wasi-audio-wasmtime) uses:

- `cpal` 0.18 for CoreAudio / ALSA / AAudio / WASAPI
- `rtrb` for a lock-free SPSC ring buffer
- a dedicated host thread that owns the cpal `Stream`

`wasi:io/poll` is linked as component-model async transport. The `WasiCtx` has
no preopens, environment, arguments or network policy; the guest only imports
the audio contract.

## Proof

```sh
cargo build --manifest-path crates/wasi-audio-wasmtime/fixtures/tone/Cargo.toml \
  --target wasm32-wasip2 --release
cp crates/wasi-audio-wasmtime/fixtures/tone/target/wasm32-wasip2/release/wasi_audio_tone.wasm \
  crates/wasi-audio-wasmtime/fixtures/tone.wasm

cargo run -p wasi-audio-wasmtime -- crates/wasi-audio-wasmtime/fixtures/tone.wasm
# audio: ok
```

GPU-style test gate:

```sh
cargo test -p wasi-audio-wasmtime --test tone -- --ignored --nocapture
```

## Isochrone sink

`wasi:audio` output can be routed to an Isochrone UDP destination instead of a
local device. The guest still writes ordinary PCM; the host packetizes it as
48 kHz stereo L24 and sends one RTP packet per millisecond.

```rust
let host = WasiAudioHost::with_isochrone(IsochroneConfig {
    bind: "0.0.0.0:0".parse()?,
    destination: "192.168.20.13:50040".parse()?,
    ssrc: 0x536f_696c,
});
```

The same configuration is available from the harness:

```sh
wasi-audio COMPONENT.wasm \
  --isochrone-destination 192.168.20.13:50040 \
  --isochrone-bind 0.0.0.0:0 \
  --isochrone-ssrc 1399813484
```

or from a host-owned JSON file:

```json
{
  "bind": "0.0.0.0:0",
  "destination": "192.168.20.13:50040",
  "ssrc": 1399813484
}
```

```sh
wasi-audio COMPONENT.wasm --isochrone-config /etc/soil/audio.json
```

Isochrone does not mix inside Soil. Each source sends to a sink port; the
designated receiver combines streams:

```text
Studio Soil  -> 192.168.20.13:50040
Neo Soil     -> 192.168.20.13:50041
Rsspotify    -> local sink input / another stream
                         |
                  home-pi receive-mix
                         |
                   hardware_dmixer
```

The final config comes from Mycelium service discovery. Soil receives a
destination identity, not a device selection from the guest.

## Limits

- PCM output only. No capture, graph, effects or AudioWorklet.
- f32 interleaved only; the host converts to the device format.
- No per-Spore mixer, quota, or device-selection policy yet.
- Isochrone output is fixed at 48 kHz stereo L24 in this slice; other formats
  return `unsupported` rather than being silently converted.
- `cpal` is host-side. There is no guest `cpal`, `rodio`, or `kira` adapter yet.
- No browser WebAudio path; this is the non-browser WASI contract.
- Android/Fire TV qualification is still needed; cpal's AAudio backend is the
  intended host path.
