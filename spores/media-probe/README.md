# media-probe

Fixture Spore for `soil:media@0.1.0`.

It exercises:

- host playback open/play/pause/seek/status/events
- encoded chunk access, including keyframe and codec config
- bounded image info and RGBA readback

The host supplies a `MediaSource` adapter for the source id `fixture`. The Spore
never receives a URL, Unibus route, credential, decoder, or output texture.

```sh
cargo build --manifest-path spores/media-probe/Cargo.toml \
  --target wasm32-wasip2 --release
```

The checked-in `module.wasm` is the test fixture.
