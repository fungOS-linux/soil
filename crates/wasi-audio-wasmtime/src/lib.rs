//! Bounded `wasi:audio` PCM sink.
//!
//! The guest owns sources, decoding and DSP; the host owns the output device,
//! sample format conversion, final mix and clock. No cpal stream or native
//! device handle crosses the component boundary.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Sender, channel, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use isochrone::UdpSender;
use isochrone_core::{Encoding, StreamFormat};

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

/// Host-selected Isochrone destination. The guest never sees this.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct IsochroneConfig {
    pub bind: SocketAddr,
    pub destination: SocketAddr,
    pub ssrc: u32,
}

pub struct WasiAudioHost {
    table: ResourceTable,
    wasi: WasiCtx,
    isochrone: Option<IsochroneConfig>,
}

impl WasiAudioHost {
    pub fn new() -> Self {
        Self {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            isochrone: None,
        }
    }

    /// Route every `wasi:audio` output to an Isochrone UDP sink. The sink is
    /// responsible for combining source streams; a Soil guest only writes PCM.
    pub fn with_isochrone(config: IsochroneConfig) -> Self {
        Self {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            isochrone: Some(config),
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
    isochrone: Option<IsochroneSink>,
    closed: bool,
}

struct IsochroneSink {
    sender: UdpSender,
    channels: usize,
    frames_per_packet: usize,
    pending: Vec<f32>,
}

impl Output {
    fn open(
        format: playback::Format,
        isochrone: Option<&IsochroneConfig>,
    ) -> Result<Self, playback::Error> {
        if !(8_000..=192_000).contains(&format.sample_rate) || !(1..=8).contains(&format.channels) {
            return Err(playback::Error::Invalid);
        }
        if let Some(config) = isochrone {
            if format.sample_rate != 48_000 || format.channels != 2 {
                return Err(playback::Error::Unsupported);
            }
            let stream = StreamFormat {
                sample_rate_hz: 48_000,
                channels: 2,
                encoding: Encoding::L24,
                frames_per_packet: 48,
            };
            let sender = UdpSender::connect(config.bind, config.destination, stream, config.ssrc)
                .map_err(|_| playback::Error::Backend)?;
            return Ok(Self {
                producer: None,
                format,
                capacity_samples: 0,
                underruns: Arc::new(AtomicU64::new(0)),
                stop: None,
                thread: None,
                isochrone: Some(IsochroneSink {
                    sender,
                    channels: 2,
                    frames_per_packet: 48,
                    pending: Vec::with_capacity(96 * 4),
                }),
                closed: false,
            });
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
            isochrone: None,
            closed: false,
        })
    }

    fn stop(&mut self) {
        self.isochrone = None;
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
        if let Some(sink) = &self.isochrone {
            return sink.pending.len() / sink.channels;
        }
        self.producer.as_ref().map_or(0, |producer| {
            (self.capacity_samples - producer.slots()) / usize::from(self.format.channels)
        })
    }

    /// Open a sink without naming the generated `wasi:audio` types. This is the
    /// surface `soil:audio` uses; the WIT wrapper above stays a thin adapter.
    pub fn open_sink(
        format: SinkFormat,
        isochrone: Option<&IsochroneConfig>,
    ) -> Result<Self, SinkError> {
        Self::open(
            playback::Format {
                sample_rate: format.sample_rate,
                channels: format.channels,
            },
            isochrone,
        )
        .map_err(sink_error)
    }

    pub fn sink_format(&self) -> SinkFormat {
        SinkFormat {
            sample_rate: self.format.sample_rate,
            channels: self.format.channels,
        }
    }

    pub fn write_sink(&mut self, frames: &[f32]) -> Result<u32, SinkError> {
        if self.closed {
            return Err(SinkError::Closed);
        }
        let channels = usize::from(self.format.channels);
        if channels == 0 || !frames.len().is_multiple_of(channels) {
            return Err(SinkError::Invalid);
        }
        if let Some(sink) = self.isochrone.as_mut() {
            sink.pending.extend_from_slice(frames);
            let packet_samples = sink.frames_per_packet * sink.channels;
            while sink.pending.len() >= packet_samples {
                let packet: Vec<f32> = sink.pending.drain(..packet_samples).collect();
                sink.sender.send(&packet).map_err(|_| SinkError::Backend)?;
            }
            return Ok((frames.len() / channels) as u32);
        }
        let producer = self.producer.as_mut().ok_or(SinkError::Closed)?;
        let mut accepted = 0;
        for frame in frames.chunks_exact(channels) {
            if producer.slots() < channels {
                break;
            }
            for sample in frame {
                producer.push(*sample).map_err(|_| SinkError::OverBudget)?;
            }
            accepted += 1;
        }
        Ok(accepted)
    }

    pub fn drain_sink(&mut self, timeout_ms: u32) -> Result<(), SinkError> {
        if self.closed {
            return Err(SinkError::Closed);
        }
        if self.isochrone.is_some() {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms.min(2_000)));
        while self.queued_frames() > 0 {
            if Instant::now() >= deadline {
                return Err(SinkError::NotReady);
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    pub fn status_sink(&self) -> SinkStatus {
        let queued = self.queued_frames();
        let frames_per_second = u64::from(self.format.sample_rate);
        SinkStatus {
            format: self.sink_format(),
            queued_frames: queued.min(u32::MAX as usize) as u32,
            underruns: self.underruns.load(Ordering::Relaxed),
            latency_ms: (queued as u64 * 1_000)
                .checked_div(frames_per_second)
                .unwrap_or(0) as u32,
        }
    }

    pub fn close_sink(&mut self) {
        self.stop();
    }
}

/// Format for the public sink surface, independent of the generated WIT types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkFormat {
    pub sample_rate: u32,
    pub channels: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkError {
    Denied,
    NotReady,
    OverBudget,
    Invalid,
    Underrun,
    Closed,
    Unsupported,
    Backend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkStatus {
    pub format: SinkFormat,
    pub queued_frames: u32,
    pub underruns: u64,
    pub latency_ms: u32,
}

fn sink_error(error: playback::Error) -> SinkError {
    match error {
        playback::Error::Denied => SinkError::Denied,
        playback::Error::NotReady => SinkError::NotReady,
        playback::Error::OverBudget => SinkError::OverBudget,
        playback::Error::Invalid => SinkError::Invalid,
        playback::Error::Underrun => SinkError::Underrun,
        playback::Error::Closed => SinkError::Closed,
        playback::Error::Unsupported => SinkError::Unsupported,
        playback::Error::Backend => SinkError::Backend,
    }
}

fn playback_error(error: SinkError) -> playback::Error {
    match error {
        SinkError::Denied => playback::Error::Denied,
        SinkError::NotReady => playback::Error::NotReady,
        SinkError::OverBudget => playback::Error::OverBudget,
        SinkError::Invalid => playback::Error::Invalid,
        SinkError::Underrun => playback::Error::Underrun,
        SinkError::Closed => playback::Error::Closed,
        SinkError::Unsupported => playback::Error::Unsupported,
        SinkError::Backend => playback::Error::Backend,
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
        let output = Output::open(format, self.isochrone.as_ref())?;
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
        output.write_sink(&frames).map_err(playback_error)
    }

    fn drain(&mut self, output: Resource<Output>, timeout_ms: u32) -> Result<(), playback::Error> {
        let output = self
            .table
            .get_mut(&output)
            .map_err(|_| playback::Error::Closed)?;
        output.drain_sink(timeout_ms).map_err(playback_error)
    }

    fn status(&mut self, output: Resource<Output>) -> Result<playback::Status, playback::Error> {
        let output = self
            .table
            .get(&output)
            .map_err(|_| playback::Error::Closed)?;
        let status = output.status_sink();
        Ok(playback::Status {
            format: playback::Format {
                sample_rate: status.format.sample_rate,
                channels: status.format.channels,
            },
            queued_frames: status.queued_frames,
            underruns: status.underruns,
            latency_ms: status.latency_ms,
        })
    }

    fn close(&mut self, output: Resource<Output>) -> Result<(), playback::Error> {
        let output = self
            .table
            .get_mut(&output)
            .map_err(|_| playback::Error::Closed)?;
        output.close_sink();
        Ok(())
    }

    fn drop(&mut self, output: Resource<Output>) -> Result<(), wasmtime::Error> {
        if let Ok(mut output) = self.table.delete(output) {
            output.close_sink();
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
        self.run_component_with_host(bytes, WasiAudioHost::new())
    }

    pub fn run_component_with_host(&self, bytes: &[u8], host: WasiAudioHost) -> Result<u32> {
        let component = Component::new(&self.engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile audio component")?;
        let mut store = Store::new(&self.engine, host);
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
