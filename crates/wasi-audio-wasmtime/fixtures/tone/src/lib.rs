//! 440 Hz stereo tone through the draft `wasi:audio` PCM sink.

use wasi::audio::playback;

wit_bindgen::generate!({
    path: "../../wit",
    world: "audio",
    generate_all,
});

export!(Component);

struct Component;

impl Guest for Component {
    fn run() -> u32 {
        let format = playback::Format {
            sample_rate: 48_000,
            channels: 2,
        };
        let output = match playback::open(format) {
            Ok(output) => output,
            Err(_) => return 1,
        };
        let rate = output.format().map(|f| f.sample_rate).unwrap_or(48_000) as f32;
        let channels = output.format().map(|f| f.channels).unwrap_or(2) as usize;
        let frames = (rate * 0.2) as usize; // 200 ms, comfortably inside the host queue
        let mut pcm = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            let phase = 2.0 * core::f32::consts::PI * 440.0 * frame as f32 / rate;
            let sample = (phase.sin() * 0.2).clamp(-1.0, 1.0);
            for _ in 0..channels {
                pcm.push(sample);
            }
        }
        let mut written = 0usize;
        while written < pcm.len() {
            match output.write(&pcm[written..]) {
                Ok(0) => break,
                Ok(frames) => written += frames as usize * channels,
                Err(_) => return 2,
            }
        }
        if written < pcm.len() {
            return 3;
        }
        if output.drain(1_000).is_err() {
            return 4;
        }
        if output.close().is_err() {
            return 5;
        }
        0
    }
}
