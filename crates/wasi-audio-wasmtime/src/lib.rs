//! Bounded `wasi:audio` PCM sink.
//!
//! The guest owns sources, decoding and DSP; the host owns the output device,
//! sample format conversion, final mix and clock. No cpal stream or native
//! device handle crosses the component boundary.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Sender, channel, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Producer, RingBuffer};
use wasmtime::component::{Component, Linker, Resource, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "wit",
    world: "audio",
    with: {
        "wasi:audio/playback.output": Output,
    },
});

use wasi::audio::playback;

pub struct WasiAudioHost {
    table: ResourceTable,
    wasi: WasiCtx,
}

impl WasiAudioHost {
    pub fn new() -> Self {
        Self {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
        }
    }
}

impl Default for WasiAudioHost {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Output {
    producer: Option<Producer<f32>>,
    format: playback::Format,
    capacity_samples: usize,
    underruns: Arc<AtomicU64>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    closed: bool,
}

impl Output {
    fn open(format: playback::Format) -> Result<Self, playback::Error> {
        if !(8_000..=192_000).contains(&format.sample_rate) || !(1..=8).contains(&format.channels) {
            return Err(playback::Error::Invalid);
        }
        let channels = usize::from(format.channels);
        let capacity_samples = (format.sample_rate as usize * channels / 4).max(1024);
        let (producer, consumer) = RingBuffer::<f32>::new(capacity_samples);
        let underruns = Arc::new(AtomicU64::new(0));
        let (ready_tx, ready_rx) = sync_channel(1);
        let (stop_tx, stop_rx) = channel();
        let thread_underruns = Arc::clone(&underruns);
        let requested = format;
        let thread = thread::Builder::new()
            .name("wasi-audio-output".into())
            .spawn(
                move || match build_stream(requested, consumer, thread_underruns) {
                    Ok((stream, actual)) => {
                        if let Err(error) = stream.play() {
                            let _ = ready_tx.send(Err(playback::Error::Backend));
                            eprintln!("wasi-audio: play: {error}");
                            return;
                        }
                        let _ = ready_tx.send(Ok(actual));
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                },
            )
            .context("spawn audio output thread")
            .map_err(|_| playback::Error::Backend)?;
        let format = ready_rx.recv().map_err(|_| playback::Error::Backend)??;
        Ok(Self {
            producer: Some(producer),
            format,
            capacity_samples,
            underruns,
            stop: Some(stop_tx),
            thread: Some(thread),
            closed: false,
        })
    }

    fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.producer = None;
        self.closed = true;
    }

    fn queued_frames(&self) -> usize {
        self.producer.as_ref().map_or(0, |producer| {
            (self.capacity_samples - producer.slots()) / usize::from(self.format.channels)
        })
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
    }
}

fn build_stream(
    format: playback::Format,
    mut consumer: rtrb::Consumer<f32>,
    underruns: Arc<AtomicU64>,
) -> Result<(cpal::Stream, playback::Format), playback::Error> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(playback::Error::NotReady)?;
    let mut configs = device
        .supported_output_configs()
        .map_err(|_| playback::Error::Backend)?;
    let requested_rate = format.sample_rate;
    let supported = configs
        .find(|range| {
            range.channels() == u16::from(format.channels)
                && range.sample_format() == cpal::SampleFormat::F32
                && range.min_sample_rate() <= requested_rate
                && requested_rate <= range.max_sample_rate()
        })
        .ok_or(playback::Error::Unsupported)?
        .with_sample_rate(requested_rate);
    let stream_config = supported.config();
    let actual = playback::Format {
        sample_rate: stream_config.sample_rate,
        channels: stream_config.channels as u8,
    };
    let stream = device
        .build_output_stream::<f32, _, _>(
            stream_config,
            move |data, _| {
                let mut missing = false;
                for sample in data {
                    match consumer.pop() {
                        Ok(value) => *sample = value,
                        Err(_) => {
                            *sample = 0.0;
                            missing = true;
                        }
                    }
                }
                if missing {
                    underruns.fetch_add(1, Ordering::Relaxed);
                }
            },
            |error| eprintln!("wasi-audio: stream error: {error}"),
            None,
        )
        .map_err(|_| playback::Error::Backend)?;
    Ok((stream, actual))
}

impl WasiView for WasiAudioHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl playback::Host for WasiAudioHost {
    fn open(&mut self, format: playback::Format) -> Result<Resource<Output>, playback::Error> {
        let output = Output::open(format)?;
        self.table
            .push(output)
            .map_err(|_| playback::Error::OverBudget)
    }
}

impl playback::HostOutput for WasiAudioHost {
    fn format(&mut self, output: Resource<Output>) -> Result<playback::Format, playback::Error> {
        self.table
            .get(&output)
            .map(|output| output.format)
            .map_err(|_| playback::Error::Closed)
    }

    fn write(
        &mut self,
        output: Resource<Output>,
        frames: Vec<f32>,
    ) -> Result<u32, playback::Error> {
        let output = self
            .table
            .get_mut(&output)
            .map_err(|_| playback::Error::Closed)?;
        if output.closed {
            return Err(playback::Error::Closed);
        }
        let channels = usize::from(output.format.channels);
        if !frames.len().is_multiple_of(channels) {
            return Err(playback::Error::Invalid);
        }
        let producer = output.producer.as_mut().ok_or(playback::Error::Closed)?;
        let mut accepted = 0;
        for frame in frames.chunks_exact(channels) {
            if producer.slots() < channels {
                break;
            }
            for sample in frame {
                producer
                    .push(*sample)
                    .map_err(|_| playback::Error::OverBudget)?;
            }
            accepted += 1;
        }
        Ok(accepted)
    }

    fn drain(&mut self, output: Resource<Output>, timeout_ms: u32) -> Result<(), playback::Error> {
        let output = self
            .table
            .get_mut(&output)
            .map_err(|_| playback::Error::Closed)?;
        if output.closed {
            return Err(playback::Error::Closed);
        }
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms.min(2_000)));
        while output.queued_frames() > 0 {
            if Instant::now() >= deadline {
                return Err(playback::Error::NotReady);
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    fn status(&mut self, output: Resource<Output>) -> Result<playback::Status, playback::Error> {
        let output = self
            .table
            .get(&output)
            .map_err(|_| playback::Error::Closed)?;
        let queued = output.queued_frames();
        let frames_per_second = u64::from(output.format.sample_rate);
        Ok(playback::Status {
            format: output.format,
            queued_frames: queued.min(u32::MAX as usize) as u32,
            underruns: output.underruns.load(Ordering::Relaxed),
            latency_ms: (queued as u64 * 1_000)
                .checked_div(frames_per_second)
                .unwrap_or(0) as u32,
        })
    }

    fn close(&mut self, output: Resource<Output>) -> Result<(), playback::Error> {
        let output = self
            .table
            .get_mut(&output)
            .map_err(|_| playback::Error::Closed)?;
        output.stop();
        Ok(())
    }

    fn drop(&mut self, output: Resource<Output>) -> Result<(), wasmtime::Error> {
        if let Ok(mut output) = self.table.delete(output) {
            output.stop();
        }
        Ok(())
    }
}

pub struct AudioRuntime {
    engine: Engine,
    linker: Linker<WasiAudioHost>,
}

impl AudioRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config).map_err(anyhow::Error::from)?;
        let mut linker = Linker::new(&engine);
        // `wasi:io/poll` is component-model async transport, not an authority.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(anyhow::Error::from)
            .context("link wasi:io polling")?;
        Audio::add_to_linker::<WasiAudioHost, wasmtime::component::HasSelf<WasiAudioHost>>(
            &mut linker,
            |state| state,
        )
        .map_err(anyhow::Error::from)
        .context("link wasi:audio")?;
        Ok(Self { engine, linker })
    }

    pub fn run_component(&self, bytes: &[u8]) -> Result<u32> {
        let component = Component::new(&self.engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile audio component")?;
        let mut store = Store::new(&self.engine, WasiAudioHost::new());
        let instance = Audio::instantiate(&mut store, &component, &self.linker)
            .map_err(anyhow::Error::from)
            .context("instantiate audio component")?;
        instance
            .call_run(&mut store)
            .map_err(anyhow::Error::from)
            .context("run audio component")
    }
}

pub fn run_component_blocking(bytes: &[u8]) -> Result<u32> {
    AudioRuntime::new()?.run_component(bytes)
}

#[cfg(test)]
mod tests {
    const AUDIO_WIT: &str = include_str!("../../../wit/wasi-audio.wit");

    #[test]
    fn pins_the_draft_wasi_audio_contract() {
        assert!(AUDIO_WIT.contains("package wasi:audio@0.1.0;"));
        assert!(AUDIO_WIT.contains("resource output"));
        assert!(AUDIO_WIT.contains("write: func(frames: list<f32>)"));
        assert!(AUDIO_WIT.contains("drain: func(timeout-ms: u32)"));
    }
}
