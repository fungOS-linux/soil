# cpal tone fixture

An ordinary cpal program, compiled for `wasm32-wasip1-threads`:

```sh
cargo build --manifest-path crates/soil-process/fixtures/cpal-tone/Cargo.toml \
  --target wasm32-wasip1-threads --release
cp crates/soil-process/fixtures/cpal-tone/target/wasm32-wasip1-threads/release/soil-cpal-tone.wasm \
  crates/soil-process/fixtures/cpal-tone.wasm
cargo run -p soil-process -- crates/soil-process/fixtures/cpal-tone.wasm
```

The guest uses cpal's default host. The fork adds a Soil host whose
`play()` starts a guest callback thread; that thread pushes PCM through the
`soil_audio` imports. The host cpal callback drains the ring and never enters
Wasm.
