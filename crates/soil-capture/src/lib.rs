//! Host-side capture adapters for `soil:mic` and `soil:camera`.
//!
//! The guest contracts are bounded and polled. These adapters own the platform
//! device, convert samples/frames to the contract shapes, and expose fixture
//! sources for tests. A native backend can be swapped without changing the
//! guest ABI.

pub mod camera;
pub mod fixture;
pub mod mic;

use std::fmt;
use std::time::Duration;

/// One read may not ask for more than this many microphone frames.
pub const MAX_MIC_FRAMES: u32 = 8192;

/// One camera frame may not exceed this many bytes.
pub const MAX_CAMERA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureError {
    Denied,
    NotFound,
    NotReady,
    Unsupported,
    OverBudget,
    Invalid,
    Closed,
    Backend,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Denied => "capture denied",
            Self::NotFound => "capture device not found",
            Self::NotReady => "capture device not ready",
            Self::Unsupported => "capture format unsupported",
            Self::OverBudget => "capture request over budget",
            Self::Invalid => "invalid capture request",
            Self::Closed => "capture session closed",
            Self::Backend => "capture backend failed",
        };
        f.write_str(text)
    }
}

impl std::error::Error for CaptureError {}

/// A poll timeout is a request, not a promise. `None` means no data arrived
/// before the deadline; it is not an error and not a zero-length frame.
pub type CaptureResult<T> = Result<T, CaptureError>;

pub fn timeout(duration: Duration) -> Duration {
    duration
}
