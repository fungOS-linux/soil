//! Deterministic sources for tests and headless probes.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use crate::camera::{CameraFormat, CameraFrame, CameraSession, CameraSource, PixelFormat};
use crate::mic::{MicChunk, MicFormat, MicSession, MicSource};
use crate::{CaptureError, CaptureResult, MAX_MIC_FRAMES};

#[derive(Debug, Clone, Default)]
pub struct FixtureMicSource {
    chunks: Vec<MicChunk>,
}

impl FixtureMicSource {
    pub fn new(chunks: Vec<MicChunk>) -> Self {
        Self { chunks }
    }

    pub fn tone(sample_rate: u32, frames: u32, amplitude: f32) -> Self {
        let samples = (0..frames)
            .map(|index| {
                let phase = 2.0 * std::f32::consts::PI * 440.0 * index as f32 / sample_rate as f32;
                phase.sin() * amplitude
            })
            .collect();
        Self::new(vec![MicChunk {
            sample_rate,
            channels: 1,
            frames,
            samples,
        }])
    }
}

impl MicSource for FixtureMicSource {
    fn open(&self, _purpose: &str) -> CaptureResult<Box<dyn MicSession>> {
        Ok(Box::new(FixtureMic {
            chunks: self.chunks.clone().into(),
            format: MicFormat {
                sample_rate: self
                    .chunks
                    .first()
                    .map_or(16_000, |chunk| chunk.sample_rate),
                channels: self.chunks.first().map_or(1, |chunk| chunk.channels),
            },
        }))
    }
}

struct FixtureMic {
    chunks: VecDeque<MicChunk>,
    format: MicFormat,
}

impl MicSession for FixtureMic {
    fn format(&self) -> MicFormat {
        self.format
    }

    fn read(&mut self, max_frames: u32, _timeout: Duration) -> CaptureResult<Option<MicChunk>> {
        if max_frames == 0 || max_frames > MAX_MIC_FRAMES {
            return Err(CaptureError::Invalid);
        }
        Ok(self.chunks.pop_front())
    }

    fn close(&mut self) -> CaptureResult<()> {
        self.chunks.clear();
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FixtureCameraSource {
    frames: Vec<CameraFrame>,
}

impl FixtureCameraSource {
    pub fn new(frames: Vec<CameraFrame>) -> Self {
        Self { frames }
    }

    pub fn solid(width: u32, height: u32, rgba: [u8; 4], count: usize) -> Self {
        let format = CameraFormat {
            width,
            height,
            pixel_format: PixelFormat::Rgba8,
        };
        let bytes = width as usize * height as usize * 4;
        let mut data = Vec::with_capacity(bytes);
        for _ in 0..width as usize * height as usize {
            data.extend_from_slice(&rgba);
        }
        let frames = (0..count)
            .map(|index| CameraFrame {
                sequence: index as u64 + 1,
                soil_time_ms: index as u64 * 1_000,
                format,
                data: data.clone(),
            })
            .collect();
        Self { frames }
    }
}

impl CameraSource for FixtureCameraSource {
    fn open(&self, _purpose: &str) -> CaptureResult<Box<dyn CameraSession>> {
        let frames = self.frames.clone();
        let format = frames.first().map_or(
            CameraFormat {
                width: 1,
                height: 1,
                pixel_format: PixelFormat::Rgba8,
            },
            |frame| frame.format,
        );
        Ok(Box::new(FixtureCamera {
            frames: Mutex::new(frames.into()),
            format,
        }))
    }
}

struct FixtureCamera {
    frames: Mutex<VecDeque<CameraFrame>>,
    format: CameraFormat,
}

impl CameraSession for FixtureCamera {
    fn format(&self) -> CameraFormat {
        self.format
    }

    fn next_frame(&mut self, _timeout: Duration) -> CaptureResult<Option<CameraFrame>> {
        Ok(self
            .frames
            .lock()
            .map_err(|_| CaptureError::Backend)?
            .pop_front())
    }

    fn close(&mut self) -> CaptureResult<()> {
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
        Ok(())
    }
}
