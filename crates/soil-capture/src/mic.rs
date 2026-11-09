//! Microphone capture.
//!
//! `CpalMicSource` uses the default input device and keeps the cpal stream on a
//! dedicated thread so the guest-facing session stays `Send`. Samples are
//! converted to interleaved f32, matching `soil:mic`'s PCM record.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};

use crate::{CaptureError, CaptureResult, MAX_MIC_FRAMES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicFormat {
    pub sample_rate: u32,
    pub channels: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MicChunk {
    pub sample_rate: u32,
    pub channels: u8,
    pub frames: u32,
    pub samples: Vec<f32>,
}

pub trait MicSession: Send {
    fn format(&self) -> MicFormat;
    fn read(&mut self, max_frames: u32, timeout: Duration) -> CaptureResult<Option<MicChunk>>;
    fn close(&mut self) -> CaptureResult<()>;
}

pub trait MicSource: Send + Sync {
    /// `purpose` is user-facing consent text. A backend that has no consent UI
    /// may ignore it; the host still must show its capture indicator.
    fn open(&self, purpose: &str) -> CaptureResult<Box<dyn MicSession>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CpalMicSource;

impl CpalMicSource {
    pub fn new() -> Self {
        Self
    }
}

impl MicSource for CpalMicSource {
    fn open(&self, _purpose: &str) -> CaptureResult<Box<dyn MicSession>> {
        Ok(Box::new(CpalMicSession::open()?))
    }
}

struct CpalMicSession {
    format: MicFormat,
    rx: Receiver<Vec<f32>>,
    stop: Option<mpsc::Sender<()>>,
    pending: VecDeque<f32>,
    error: Arc<Mutex<Option<String>>>,
    closed: bool,
}

impl CpalMicSession {
    fn open() -> CaptureResult<Self> {
        let (tx, rx) = mpsc::sync_channel(32);
        let (stop, stop_rx) = mpsc::channel();
        let (format_tx, format_rx) = mpsc::sync_channel(1);
        let error = Arc::new(Mutex::new(None));
        let thread_error = Arc::clone(&error);

        std::thread::Builder::new()
            .name("soil-mic".into())
            .spawn(move || {
                let host = cpal::default_host();
                let device = match host.default_input_device() {
                    Some(device) => device,
                    None => {
                        let _ = format_tx.send(Err(CaptureError::NotFound));
                        return;
                    }
                };
                let supported = match device.default_input_config() {
                    Ok(supported) => supported,
                    Err(_) => {
                        let _ = format_tx.send(Err(CaptureError::NotReady));
                        return;
                    }
                };
                let sample_format = supported.sample_format();
                let config: cpal::StreamConfig = supported.into();
                let format = match u8::try_from(config.channels) {
                    Ok(channels) => MicFormat {
                        sample_rate: config.sample_rate,
                        channels,
                    },
                    Err(_) => {
                        let _ = format_tx.send(Err(CaptureError::Unsupported));
                        return;
                    }
                };
                let channels = config.channels as usize;
                let stream = match sample_format {
                    SampleFormat::I8 => build_input::<i8>(&device, config.clone(), tx, channels),
                    SampleFormat::I16 => build_input::<i16>(&device, config.clone(), tx, channels),
                    SampleFormat::I32 => build_input::<i32>(&device, config.clone(), tx, channels),
                    SampleFormat::I64 => build_input::<i64>(&device, config.clone(), tx, channels),
                    SampleFormat::U8 => build_input::<u8>(&device, config.clone(), tx, channels),
                    SampleFormat::U16 => build_input::<u16>(&device, config.clone(), tx, channels),
                    SampleFormat::U32 => build_input::<u32>(&device, config.clone(), tx, channels),
                    SampleFormat::U64 => build_input::<u64>(&device, config.clone(), tx, channels),
                    SampleFormat::F32 => build_input::<f32>(&device, config.clone(), tx, channels),
                    SampleFormat::F64 => build_input::<f64>(&device, config.clone(), tx, channels),
                    other => {
                        set_error(
                            &thread_error,
                            format!("unsupported sample format {other:?}"),
                        );
                        let _ = format_tx.send(Err(CaptureError::Unsupported));
                        return;
                    }
                };
                let stream = match stream {
                    Ok(stream) => stream,
                    Err(error) => {
                        set_error(&thread_error, error);
                        let _ = format_tx.send(Err(CaptureError::Backend));
                        return;
                    }
                };
                if let Err(error) = stream.play() {
                    set_error(&thread_error, error.to_string());
                    let _ = format_tx.send(Err(CaptureError::Backend));
                    return;
                }
                if format_tx.send(Ok(format)).is_err() {
                    return;
                }
                let _ = stop_rx.recv();
                drop(stream);
            })
            .map_err(|_| CaptureError::Backend)?;

        let format = format_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| CaptureError::NotReady)??;
        Ok(Self {
            format,
            rx,
            stop: Some(stop),
            pending: VecDeque::new(),
            error,
            closed: false,
        })
    }
}

impl MicSession for CpalMicSession {
    fn format(&self) -> MicFormat {
        self.format
    }

    fn read(&mut self, max_frames: u32, timeout: Duration) -> CaptureResult<Option<MicChunk>> {
        if self.closed {
            return Err(CaptureError::Closed);
        }
        if max_frames == 0 || max_frames > MAX_MIC_FRAMES {
            return Err(CaptureError::Invalid);
        }
        let channels = usize::from(self.format.channels.max(1));
        let target = usize::try_from(max_frames)
            .map_err(|_| CaptureError::Invalid)?
            .saturating_mul(channels);
        let deadline = Instant::now() + timeout;

        while self.pending.len() < target {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(remaining) {
                Ok(samples) => self.pending.extend(samples),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    self.closed = true;
                    return Err(CaptureError::Closed);
                }
            }
        }

        if self.pending.is_empty() {
            if self
                .error
                .lock()
                .ok()
                .and_then(|error| error.clone())
                .is_some()
            {
                return Err(CaptureError::Backend);
            }
            return Ok(None);
        }

        let complete = (self.pending.len() / channels) * channels;
        let take = complete.min(target);
        let frames = u32::try_from(take / channels).map_err(|_| CaptureError::OverBudget)?;
        let samples = self.pending.drain(..take).collect();
        Ok(Some(MicChunk {
            sample_rate: self.format.sample_rate,
            channels: self.format.channels,
            frames,
            samples,
        }))
    }

    fn close(&mut self) -> CaptureResult<()> {
        if !self.closed {
            self.closed = true;
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
        }
        Ok(())
    }
}

impl Drop for CpalMicSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn build_input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    tx: SyncSender<Vec<f32>>,
    _channels: usize,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                if data.is_empty() {
                    return;
                }
                let mut samples = Vec::with_capacity(data.len());
                for sample in data {
                    samples.push(f32::from_sample(*sample));
                }
                // A full channel means the consumer is behind. Dropping the
                // chunk is better than blocking the audio callback.
                let _ = tx.try_send(samples);
            },
            |error| eprintln!("soil-mic: stream error: {error}"),
            None,
        )
        .map_err(|error| error.to_string())
}

fn set_error(slot: &Arc<Mutex<Option<String>>>, error: String) {
    if let Ok(mut slot) = slot.lock() {
        *slot = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureMic {
        chunks: Vec<MicChunk>,
    }

    impl MicSession for FixtureMic {
        fn format(&self) -> MicFormat {
            MicFormat {
                sample_rate: 16_000,
                channels: 1,
            }
        }

        fn read(&mut self, max_frames: u32, _timeout: Duration) -> CaptureResult<Option<MicChunk>> {
            if max_frames == 0 || max_frames > MAX_MIC_FRAMES {
                return Err(CaptureError::Invalid);
            }
            Ok(self.chunks.pop())
        }

        fn close(&mut self) -> CaptureResult<()> {
            Ok(())
        }
    }

    #[test]
    fn fixture_chunks_are_bounded_and_typed() {
        let mut mic = FixtureMic {
            chunks: vec![MicChunk {
                sample_rate: 16_000,
                channels: 1,
                frames: 320,
                samples: vec![0.0; 320],
            }],
        };
        let chunk = mic.read(320, Duration::ZERO).unwrap().unwrap();
        assert_eq!(chunk.frames, 320);
        assert_eq!(chunk.channels, 1);
        assert_eq!(chunk.samples.len(), 320);
        assert!(mic.read(320, Duration::ZERO).unwrap().is_none());
    }

    #[test]
    fn read_rejects_zero_and_oversized_requests() {
        let mut mic = FixtureMic { chunks: vec![] };
        assert_eq!(mic.read(0, Duration::ZERO), Err(CaptureError::Invalid));
        assert_eq!(
            mic.read(MAX_MIC_FRAMES + 1, Duration::ZERO),
            Err(CaptureError::Invalid)
        );
    }
}
