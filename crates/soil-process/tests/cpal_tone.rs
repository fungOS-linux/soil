//! Ordinary cpal callback code compiled to `wasm32-wasip1-threads`.
//!
//! Requires a native output device. Run explicitly with:
//!   cargo test -p soil-process --test cpal_tone -- --ignored --nocapture

#[test]
#[ignore = "requires a native audio output device"]
fn cpal_callback_runs_on_a_guest_thread() {
    let bytes = include_bytes!("../fixtures/cpal-tone.wasm");
    let runtime = soil_process::ProcessRuntime::new(bytes).expect("compile process");
    runtime.run().expect("run cpal process");
    let stats = runtime.audio_stats();
    eprintln!("{stats:?}");
    assert!(stats.frames_written > 0, "guest never wrote PCM");
    assert!(
        stats.frames_played > 0,
        "host cpal callback never drained PCM"
    );
    assert!(
        stats.underruns < stats.frames_written / 4,
        "excessive underruns: {stats:?}"
    );
}
