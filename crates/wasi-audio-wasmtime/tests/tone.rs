//! Real guest -> draft `wasi:audio` -> cpal output proof.
//!
//! Requires an output device. Run explicitly:
//!   cargo test -p wasi-audio-wasmtime --test tone -- --ignored --nocapture

#[test]
#[ignore = "requires an audio output device"]
fn tone_guest_completes_without_error() {
    let component = include_bytes!("../fixtures/tone.wasm");
    let status = wasi_audio_wasmtime::run_component_blocking(component).expect("guest run");
    assert_eq!(status, 0, "guest reported audio failure");
}
