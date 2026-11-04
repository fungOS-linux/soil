//! cpal host for Soil threaded processes.
//!
//! On `wasm32-wasip1-threads`, `host()` returns a cpal `Host` backed by the
//! `soil_audio` process imports. On native targets it returns cpal's normal
//! default host, so the same application line works in both builds:
//!
//! ```ignore
//! let host = cpal_soil::host();
//! ```

#[cfg(target_os = "wasi")]
mod wasi;

#[cfg(target_os = "wasi")]
pub use wasi::host;

/// Return the Soil host on threaded WASI, or cpal's native default elsewhere.
#[cfg(not(target_os = "wasi"))]
pub fn host() -> cpal::Host {
    cpal::default_host()
}
