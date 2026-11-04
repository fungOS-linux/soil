//! CPAL host backed by Soil's `soil_audio` process imports.
//!
//! The guest owns the callback thread. The host owns the device and only drains
//! a bounded PCM ring; no host audio callback ever enters Wasm.

use std::{
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use cpal::{
    Data, DeviceDescription, DeviceDescriptionBuilder, DeviceId, Error, ErrorKind, FrameCount,
    HostId, InputCallbackInfo, OutputCallbackInfo, OutputStreamTimestamp, SampleFormat,
    StreamConfig, StreamInstant, SupportedBufferSize, SupportedStreamConfig,
    SupportedStreamConfigRange,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

#[link(wasm_import_module = "soil_audio")]
unsafe extern "C" {
    fn soil_audio_open(sample_rate: u32, channels: u16) -> i32;
    fn soil_audio_write(handle: i32, samples: *const f32, frames: u32) -> i32;
    fn soil_audio_close(handle: i32);
}

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 1024;

#[derive(Clone, Debug)]
pub struct Host;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Device {
    id: DeviceId,
}

impl Device {
    fn new() -> Self {
        Self {
            id: DeviceId::new(HostId::Custom, "soil-default-output"),
        }
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Soil default output")
    }
}

impl DeviceTrait for Device {
    type SupportedInputConfigs = std::vec::IntoIter<SupportedStreamConfigRange>;
    type SupportedOutputConfigs = std::vec::IntoIter<SupportedStreamConfigRange>;
    type Stream = Stream;

    fn description(&self) -> Result<DeviceDescription, Error> {
        Ok(DeviceDescriptionBuilder::new("Soil default output")
            .driver("soil_audio")
            .direction(cpal::DeviceDirection::Output)
            .build())
    }

    fn id(&self) -> Result<DeviceId, Error> {
        Ok(self.id.clone())
    }

    fn supported_input_configs(&self) -> Result<Self::SupportedInputConfigs, Error> {
        Ok(Vec::new().into_iter())
    }

    fn supported_output_configs(&self) -> Result<Self::SupportedOutputConfigs, Error> {
        Ok(vec![SupportedStreamConfigRange::new(
            CHANNELS,
            SAMPLE_RATE,
            SAMPLE_RATE,
            SupportedBufferSize::Range {
                min: BLOCK_FRAMES as FrameCount,
                max: BLOCK_FRAMES as FrameCount,
            },
            SampleFormat::F32,
        )]
        .into_iter())
    }

    fn default_input_config(&self) -> Result<SupportedStreamConfig, Error> {
        Err(Error::new(ErrorKind::UnsupportedOperation))
    }

    fn default_output_config(&self) -> Result<SupportedStreamConfig, Error> {
        Ok(SupportedStreamConfig::new(
            CHANNELS,
            SAMPLE_RATE,
            SupportedBufferSize::Range {
                min: BLOCK_FRAMES as FrameCount,
                max: BLOCK_FRAMES as FrameCount,
            },
            SampleFormat::F32,
        ))
    }

    fn build_input_stream_raw<D, E>(
        &self,
        _config: StreamConfig,
        _sample_format: SampleFormat,
        _data_callback: D,
        _error_callback: E,
        _timeout: Option<Duration>,
    ) -> Result<Self::Stream, Error>
    where
        D: FnMut(&Data, &InputCallbackInfo) + Send + 'static,
        E: FnMut(Error) + Send + 'static,
    {
        Err(Error::new(ErrorKind::UnsupportedOperation))
    }

    fn build_output_stream_raw<D, E>(
        &self,
        config: StreamConfig,
        sample_format: SampleFormat,
        data_callback: D,
        error_callback: E,
        _timeout: Option<Duration>,
    ) -> Result<Self::Stream, Error>
    where
        D: FnMut(&mut Data, &OutputCallbackInfo) + Send + 'static,
        E: FnMut(Error) + Send + 'static,
    {
        if sample_format != SampleFormat::F32 {
            return Err(Error::new(ErrorKind::UnsupportedConfig));
        }
        if config.channels != CHANNELS {
            return Err(Error::new(ErrorKind::UnsupportedConfig));
        }
        let handle = unsafe { soil_audio_open(config.sample_rate, config.channels) };
        if handle < 0 {
            return Err(Error::new(ErrorKind::BackendError));
        }
        Ok(Stream {
            state: Arc::new(StreamState {
                handle,
                sample_rate: config.sample_rate,
                channels: usize::from(config.channels),
                callback: Mutex::new(Box::new(data_callback)),
                error_callback: Mutex::new(Box::new(error_callback)),
                stop: AtomicBool::new(true),
                thread: Mutex::new(None),
                start: Instant::now(),
            }),
        })
    }
}

impl HostTrait for Host {
    type Devices = std::vec::IntoIter<Device>;
    type Device = Device;

    fn is_available() -> bool {
        true
    }

    fn devices(&self) -> Result<Self::Devices, Error> {
        Ok(vec![Device::new()].into_iter())
    }

    fn default_input_device(&self) -> Option<Self::Device> {
        None
    }

    fn default_output_device(&self) -> Option<Self::Device> {
        Some(Device::new())
    }
}

type OutputCallback = Box<dyn FnMut(&mut Data, &OutputCallbackInfo) + Send + 'static>;
type ErrorCallback = Box<dyn FnMut(Error) + Send + 'static>;

struct StreamState {
    handle: i32,
    sample_rate: u32,
    channels: usize,
    callback: Mutex<OutputCallback>,
    error_callback: Mutex<ErrorCallback>,
    stop: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
    start: Instant,
}

impl Drop for StreamState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Stop the device before waiting for the guest callback thread. The
        // guest may still be blocked in `soil_audio_write`; closing first
        // turns that into backpressure instead of a visible tail underrun.
        unsafe { soil_audio_close(self.handle) };
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
pub struct Stream {
    state: Arc<StreamState>,
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream").finish_non_exhaustive()
    }
}

impl StreamTrait for Stream {
    fn play(&self) -> Result<(), Error> {
        let mut slot = self.state.thread.lock().unwrap();
        if slot.is_some() {
            self.state.stop.store(false, Ordering::Release);
            return Ok(());
        }
        self.state.stop.store(false, Ordering::Release);
        let state = Arc::clone(&self.state);
        let thread = thread::Builder::new()
            .name("soil-cpal-callback".into())
            .spawn(move || run_callback(state))
            .map_err(|_| Error::new(ErrorKind::ResourceExhausted))?;
        *slot = Some(thread);
        Ok(())
    }

    fn pause(&self) -> Result<(), Error> {
        self.state.stop.store(true, Ordering::Release);
        if let Some(thread) = self.state.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
        Ok(())
    }

    fn buffer_size(&self) -> Result<FrameCount, Error> {
        Ok(BLOCK_FRAMES as FrameCount)
    }

    fn now(&self) -> StreamInstant {
        StreamInstant::from_nanos(self.state.start.elapsed().as_nanos() as u64)
    }
}

fn run_callback(state: Arc<StreamState>) {
    let frames = BLOCK_FRAMES;
    let samples = frames * state.channels;
    let period =
        Duration::from_nanos((frames as u64 * 1_000_000_000) / u64::from(state.sample_rate.max(1)));
    let mut buffer = vec![0.0f32; samples];
    let mut next_tick = Instant::now() + period;
    while !state.stop.load(Ordering::Acquire) {
        {
            let mut callback = state.callback.lock().unwrap();
            let mut data =
                unsafe { Data::from_parts(buffer.as_mut_ptr().cast(), samples, SampleFormat::F32) };
            let now = StreamInstant::from_nanos(state.start.elapsed().as_nanos() as u64);
            let info = OutputCallbackInfo::new(OutputStreamTimestamp {
                callback: now,
                playback: now + period,
            });
            callback(&mut data, &info);
        }

        let mut offset = 0usize;
        while offset < samples && !state.stop.load(Ordering::Acquire) {
            let remaining_frames = ((samples - offset) / state.channels) as u32;
            let accepted = unsafe {
                soil_audio_write(state.handle, buffer[offset..].as_ptr(), remaining_frames)
            };
            if accepted < 0 {
                let mut error = state.error_callback.lock().unwrap();
                error(Error::new(ErrorKind::BackendError));
                state.stop.store(true, Ordering::Release);
                break;
            } else if accepted == 0 {
                thread::sleep(Duration::from_millis(1));
            } else {
                offset += accepted as usize * state.channels;
            }
        }

        // Keep an absolute deadline. Sleeping for `period - elapsed` lets
        // scheduler overshoot accumulate, slowly starving the host ring.
        let now = Instant::now();
        if next_tick > now {
            thread::sleep(next_tick - now);
        }
        next_tick += period;
        if next_tick < now {
            // We fell more than one period behind; resynchronise instead of
            // trying to catch up in a burst.
            next_tick = now + period;
        }
    }
}

cpal::assert_stream_send!(Stream);
cpal::assert_stream_sync!(Stream);

/// Build the cpal host that routes output through Soil's process imports.
pub fn host() -> cpal::Host {
    cpal::Host::from(cpal::platform::CustomHost::from_host(Host))
}
