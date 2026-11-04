//! Host-owned cpal output with a bounded guest-write ring.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Sender, channel, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Producer, RingBuffer};

const MAX_HANDLES: i32 = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioStats {
    pub frames_written: u64,
    pub frames_played: u64,
    pub underruns: u64,
}

pub struct AudioHost {
    next: AtomicI32,
    outputs: Mutex<HashMap<i32, Output>>,
    frames_written: AtomicU64,
    frames_played: AtomicU64,
    underruns: AtomicU64,
}

impl AudioHost {
    pub fn new() -> Self {
        Self {
            next: AtomicI32::new(1),
            outputs: Mutex::new(HashMap::new()),
            frames_written: AtomicU64::new(0),
            frames_played: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
        }
    }

    pub fn stats(&self) -> AudioStats {
        let mut frames_played = self.frames_played.load(Ordering::Relaxed);
        let mut underruns = self.underruns.load(Ordering::Relaxed);
        for output in self
            .outputs
            .lock()
            .expect("audio outputs poisoned")
            .values()
        {
            frames_played += output.stats.frames_played.load(Ordering::Relaxed);
            underruns += output.stats.underruns.load(Ordering::Relaxed);
        }
        AudioStats {
            frames_written: self.frames_written.load(Ordering::Relaxed),
            frames_played,
            underruns,
        }
    }

    pub fn open(&self, sample_rate: u32, channels: u16) -> Result<i32> {
        if !(8_000..=192_000).contains(&sample_rate) || !(1..=8).contains(&channels) {
            bail!("invalid audio format {sample_rate} Hz / {channels} ch");
        }
        let handle = self.next.fetch_add(1, Ordering::Relaxed);
        if handle > MAX_HANDLES {
            bail!("audio handle budget exhausted");
        }
        let output = Output::open(
            sample_rate,
            channels,
            Arc::new(PlaybackStats {
                frames_played: AtomicU64::new(0),
                underruns: AtomicU64::new(0),
            }),
        )?;
        self.outputs
            .lock()
            .expect("audio outputs poisoned")
            .insert(handle, output);
        Ok(handle)
    }

    pub fn channels(&self, handle: i32) -> Option<usize> {
        self.outputs
            .lock()
            .expect("audio outputs poisoned")
            .get(&handle)
            .map(|output| output.channels)
    }

    pub fn write(&self, handle: i32, samples: &[f32], channels: usize) -> usize {
        let mut outputs = self.outputs.lock().expect("audio outputs poisoned");
        let Some(output) = outputs.get_mut(&handle) else {
            return 0;
        };
        let frames = samples.len() / channels;
        let mut accepted = 0;
        for frame in samples.chunks_exact(channels).take(frames) {
            if output.producer.push(frame[0]).is_err() {
                break;
            }
            let mut full = false;
            for sample in &frame[1..] {
                if output.producer.push(*sample).is_err() {
                    full = true;
                    break;
                }
            }
            if full {
                break;
            }
            accepted += 1;
        }
        self.frames_written
            .fetch_add(accepted as u64, Ordering::Relaxed);
        output
            .filled
            .fetch_add(accepted * channels, Ordering::Relaxed);
        accepted
    }

    pub fn close(&self, handle: i32) {
        if let Some(mut output) = self
            .outputs
            .lock()
            .expect("audio outputs poisoned")
            .remove(&handle)
        {
            output.stop();
            self.frames_played.fetch_add(
                output.stats.frames_played.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            self.underruns.fetch_add(
                output.stats.underruns.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
    }
}

impl Default for AudioHost {
    fn default() -> Self {
        Self::new()
    }
}

struct PlaybackStats {
    frames_played: AtomicU64,
    underruns: AtomicU64,
}

struct Output {
    producer: Producer<f32>,
    channels: usize,
    filled: Arc<AtomicUsize>,
    stats: Arc<PlaybackStats>,
    stopping: Arc<AtomicBool>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Output {
    fn open(sample_rate: u32, channels: u16, stats: Arc<PlaybackStats>) -> Result<Self> {
        let capacity = (sample_rate as usize * usize::from(channels) / 4).max(1024);
        let (producer, consumer) = RingBuffer::<f32>::new(capacity);
        let (ready_tx, ready_rx) = sync_channel(1);
        let (stop_tx, stop_rx) = channel();
        let thread_stats = Arc::clone(&stats);
        let filled = Arc::new(AtomicUsize::new(0));
        let thread_filled = Arc::clone(&filled);
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_stopping = Arc::clone(&stopping);
        let prefill_samples = (sample_rate as usize / 8).max(2048) * usize::from(channels);
        let thread = thread::Builder::new()
            .name("soil-cpal-device".into())
            .spawn(move || {
                match build_stream(
                    sample_rate,
                    channels,
                    consumer,
                    thread_stats,
                    thread_stopping,
                ) {
                    Ok(stream) => {
                        // Build the device stream stopped, let the guest prefill
                        // a few callback blocks, then start draining. This keeps
                        // startup jitter out of the first device callback.
                        let _ = ready_tx.send(Ok(()));
                        loop {
                            if stop_rx.try_recv().is_ok() {
                                return;
                            }
                            if thread_filled.load(Ordering::Acquire) >= prefill_samples {
                                break;
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                        if let Err(error) = stream.play() {
                            eprintln!("soil-cpal device play: {error}");
                            return;
                        }
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("{error:#}")));
                    }
                }
            })
            .context("spawn host audio thread")?;
        ready_rx
            .recv()
            .context("audio device thread exited")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            producer,
            channels: usize::from(channels),
            filled,
            stats,
            stopping,
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }

    fn stop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
    }
}

fn build_stream(
    sample_rate: u32,
    channels: u16,
    mut consumer: rtrb::Consumer<f32>,
    stats: Arc<PlaybackStats>,
    stopping: Arc<AtomicBool>,
) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no default output device")?;
    let config = cpal::SupportedStreamConfig::new(
        channels,
        sample_rate,
        cpal::SupportedBufferSize::Range { min: 64, max: 4096 },
        cpal::SampleFormat::F32,
    );
    let stream_config = config.config();
    let stream = device
        .build_output_stream::<f32, _, _>(
            stream_config,
            move |data, _| {
                let mut played = 0u64;
                let mut missing = false;
                for sample in data {
                    match consumer.pop() {
                        Ok(value) => {
                            *sample = value;
                            played += 1;
                        }
                        Err(_) => {
                            *sample = 0.0;
                            missing = true;
                        }
                    }
                }
                stats
                    .frames_played
                    .fetch_add(played / u64::from(channels), Ordering::Relaxed);
                if missing && !stopping.load(Ordering::Acquire) {
                    stats.underruns.fetch_add(1, Ordering::Relaxed);
                }
            },
            |error| eprintln!("soil-cpal stream error: {error}"),
            None,
        )
        .context("build host output stream")?;
    Ok(stream)
}
