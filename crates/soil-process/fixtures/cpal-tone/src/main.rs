//! Ordinary cpal source, compiled for `wasm32-wasip1-threads`.
//!
//! No Soil-specific code: cpal's default host is the Soil host in the fork.

use std::{f32::consts::TAU, thread, time::Duration};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

fn main() {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("default output device");
    let config = device
        .default_output_config()
        .expect("default output config");
    let sample_rate = config.sample_rate() as f32;
    let channels = usize::from(config.channels());
    let mut phase = 0.0f32;

    let stream = device
        .build_output_stream(
            config.config(),
            move |data: &mut [f32], _| {
                for frame in data.chunks_mut(channels) {
                    let sample = (phase * TAU).sin() * 0.12;
                    phase = (phase + 440.0 / sample_rate).fract();
                    frame.fill(sample);
                }
            },
            |error| eprintln!("cpal stream error: {error}"),
            None,
        )
        .expect("build output stream");

    stream.play().expect("play");
    thread::sleep(Duration::from_millis(1500));
    drop(stream);
}
