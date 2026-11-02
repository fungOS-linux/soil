//! Guest-side convenience layer for the draft `wasi:audio` PCM sink.
//!
//! This crate owns only guest-side rendering and backpressure. The host still
//! owns the device, clock, final mix and conversion.

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
    generate_all,
});

use wasi::audio::playback;

pub use playback::{Error, Format, Status};

/// One open PCM sink.
pub struct Output {
    inner: playback::Output,
    channels: usize,
}

impl Output {
    pub fn open(sample_rate: u32, channels: u8) -> Result<Self, Error> {
        let inner = playback::open(Format {
            sample_rate,
            channels,
        })?;
        Ok(Self {
            inner,
            channels: usize::from(channels),
        })
    }

    pub fn format(&self) -> Result<Format, Error> {
        self.inner.format()
    }

    /// Push interleaved f32 frames. Returns accepted frames; zero means the
    /// host queue is full.
    pub fn write(&mut self, frames: &[f32]) -> Result<u32, Error> {
        if !frames.len().is_multiple_of(self.channels) {
            return Err(Error::Invalid);
        }
        self.inner.write(frames)
    }

    /// Push all frames, draining the host queue when it applies backpressure.
    /// The drain is host-scheduled; the guest does not invent a clock.
    pub fn write_all(&mut self, frames: &[f32]) -> Result<(), Error> {
        let mut offset = 0;
        while offset < frames.len() {
            let accepted = self.write(&frames[offset..])?;
            if accepted == 0 {
                self.inner.drain(50)?;
            } else {
                offset += accepted as usize * self.channels;
            }
        }
        Ok(())
    }

    pub fn drain(&mut self, timeout_ms: u32) -> Result<(), Error> {
        self.inner.drain(timeout_ms)
    }

    pub fn status(&mut self) -> Result<Status, Error> {
        self.inner.status()
    }

    pub fn close(self) -> Result<(), Error> {
        self.inner.close()
    }
}

/// Render a mono-valued tone into interleaved frames.
pub fn tone(
    sample_rate: u32,
    channels: u8,
    frequency_hz: f32,
    duration_ms: u32,
    amplitude: f32,
) -> Vec<f32> {
    let rate = sample_rate.max(1) as f32;
    let frames = (u64::from(sample_rate) * u64::from(duration_ms) / 1_000) as usize;
    let channels = usize::from(channels.max(1));
    let amplitude = amplitude.clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(frames.saturating_mul(channels));
    for frame in 0..frames {
        let phase = 2.0 * core::f32::consts::PI * frequency_hz * frame as f32 / rate;
        let sample = (phase.sin() * amplitude).clamp(-1.0, 1.0);
        for _ in 0..channels {
            out.push(sample);
        }
    }
    out
}

/// Convenience: open, write, drain and close one tone.
pub fn play_tone(
    sample_rate: u32,
    channels: u8,
    frequency_hz: f32,
    duration_ms: u32,
    amplitude: f32,
) -> Result<(), Error> {
    let mut output = Output::open(sample_rate, channels)?;
    output.write_all(&tone(
        sample_rate,
        channels,
        frequency_hz,
        duration_ms,
        amplitude,
    ))?;
    output.drain(1_000)?;
    output.close()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIT: &str = include_str!("../../../wit/wasi-audio.wit");

    #[test]
    fn pins_the_guest_world_and_contract() {
        assert!(WIT.contains("package wasi:audio@0.1.0;"));
        assert!(WIT.contains("world guest"));
        assert!(WIT.contains("resource output"));
    }

    #[test]
    fn tone_shape_is_interleaved_and_bounded() {
        let samples = tone(48_000, 2, 440.0, 100, 0.2);
        assert_eq!(samples.len(), 4_800 * 2);
        assert!(samples.iter().all(|s| s.abs() <= 0.2 + f32::EPSILON));
    }
}
