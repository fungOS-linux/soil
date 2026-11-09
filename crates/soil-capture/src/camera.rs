//! Low-rate camera capture.
//!
//! The first host backend is a deliberately small ffmpeg adapter: it asks
//! AVFoundation/V4L2 for one RGB frame at a time, writes fixed-size RGBA frames
//! to a pipe, and never exposes a device handle to a guest. A native AVFoundation
//! or PipeWire backend can replace it behind the same `CameraSource` trait.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::{CaptureError, CaptureResult, MAX_CAMERA_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba8,
    Mjpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraFormat {
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraFrame {
    pub sequence: u64,
    pub soil_time_ms: u64,
    pub format: CameraFormat,
    pub data: Vec<u8>,
}

pub trait CameraSession: Send {
    fn format(&self) -> CameraFormat;
    fn next_frame(&mut self, timeout: Duration) -> CaptureResult<Option<CameraFrame>>;
    fn close(&mut self) -> CaptureResult<()>;
}

pub trait CameraSource: Send + Sync {
    /// `purpose` is user-facing consent text. The backend owns the device and
    /// the host is responsible for the visible capture indicator.
    fn open(&self, purpose: &str) -> CaptureResult<Box<dyn CameraSession>>;
}

#[derive(Debug, Clone)]
pub struct FfmpegCameraSource {
    pub program: PathBuf,
    /// ffmpeg avfoundation device selector, for example `"0"` or `"Brio 101"`.
    pub device: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for FfmpegCameraSource {
    fn default() -> Self {
        Self {
            program: PathBuf::from("ffmpeg"),
            device: "0".into(),
            width: 640,
            height: 480,
            fps: 1,
        }
    }
}

impl FfmpegCameraSource {
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            ..Self::default()
        }
    }
}

impl CameraSource for FfmpegCameraSource {
    fn open(&self, _purpose: &str) -> CaptureResult<Box<dyn CameraSession>> {
        if self.width == 0 || self.height == 0 || self.fps == 0 {
            return Err(CaptureError::Invalid);
        }
        let frame_bytes = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(CaptureError::OverBudget)?;
        if frame_bytes > MAX_CAMERA_BYTES {
            return Err(CaptureError::OverBudget);
        }

        // AVFoundation rejects low input rates for many cameras. Capture at a
        // supported input rate and let ffmpeg drop to the requested output
        // rate; the host still receives at most `fps` frames per second.
        let input_fps = self.fps.max(5);
        let mut child = Command::new(&self.program)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "avfoundation",
                "-pixel_format",
                "uyvy422",
                "-framerate",
                &input_fps.to_string(),
                "-video_size",
                &format!("{}x{}", self.width, self.height),
                "-i",
                &self.device,
                "-an",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-r",
                &self.fps.to_string(),
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| CaptureError::NotFound)?;
        let stdout = child.stdout.take().ok_or(CaptureError::Backend)?;
        let format = CameraFormat {
            width: self.width,
            height: self.height,
            pixel_format: PixelFormat::Rgba8,
        };
        let (tx, rx) = mpsc::sync_channel(2);
        let started = Instant::now();
        std::thread::Builder::new()
            .name("soil-camera".into())
            .spawn(move || {
                let mut stdout = stdout;
                let mut sequence = 0_u64;
                loop {
                    let mut data = vec![0_u8; frame_bytes];
                    if stdout.read_exact(&mut data).is_err() {
                        break;
                    }
                    sequence += 1;
                    let frame = CameraFrame {
                        sequence,
                        soil_time_ms: started.elapsed().as_millis() as u64,
                        format,
                        data,
                    };
                    if tx.send(frame).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| CaptureError::Backend)?;

        Ok(Box::new(FfmpegCameraSession {
            child,
            format,
            rx,
            closed: false,
        }))
    }
}

struct FfmpegCameraSession {
    child: Child,
    format: CameraFormat,
    rx: Receiver<CameraFrame>,
    closed: bool,
}

impl CameraSession for FfmpegCameraSession {
    fn format(&self) -> CameraFormat {
        self.format
    }

    fn next_frame(&mut self, timeout: Duration) -> CaptureResult<Option<CameraFrame>> {
        if self.closed {
            return Err(CaptureError::Closed);
        }
        match self.rx.recv_timeout(timeout) {
            Ok(frame) => Ok(Some(frame)),
            Err(RecvTimeoutError::Timeout) => {
                if self.child.try_wait().ok().flatten().is_some() {
                    self.closed = true;
                    Err(CaptureError::Backend)
                } else {
                    Ok(None)
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.closed = true;
                Err(CaptureError::Backend)
            }
        }
    }

    fn close(&mut self) -> CaptureResult<()> {
        if !self.closed {
            self.closed = true;
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        Ok(())
    }
}

impl Drop for FfmpegCameraSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_budget_rejects_oversized_cameras() {
        let source = FfmpegCameraSource {
            width: 4096,
            height: 4096,
            ..FfmpegCameraSource::default()
        };
        assert!(matches!(source.open("test"), Err(CaptureError::OverBudget)));
    }

    #[test]
    fn zero_fps_is_invalid() {
        let source = FfmpegCameraSource {
            fps: 0,
            ..FfmpegCameraSource::default()
        };
        assert!(matches!(source.open("test"), Err(CaptureError::Invalid)));
    }
}
