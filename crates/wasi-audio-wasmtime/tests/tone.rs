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

#[test]
fn tone_guest_streams_packets_to_isochrone() {
    use std::net::UdpSocket;
    use std::time::Duration;

    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let destination = receiver.local_addr().unwrap();
    let host =
        wasi_audio_wasmtime::WasiAudioHost::with_isochrone(wasi_audio_wasmtime::IsochroneConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            destination,
            ssrc: 0x1234_5678,
        });
    let status = wasi_audio_wasmtime::AudioRuntime::new()
        .unwrap()
        .run_component_with_host(include_bytes!("../fixtures/tone.wasm"), host)
        .expect("guest run");
    assert_eq!(status, 0);
    let mut packet = [0u8; 2048];
    let (len, _) = receiver.recv_from(&mut packet).expect("isochrone packet");
    assert!(len >= 12 + 288, "short packet: {len}");
    assert_eq!(packet[1] & 0x7f, 97, "AES67 dynamic payload type");
}
