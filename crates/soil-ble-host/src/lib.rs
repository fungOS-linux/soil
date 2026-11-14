//! Reusable BLE host pieces for the protocol host and the desktop dev host.
//!
//! The `soil:ble` trait implementation itself stays in the binaries because
//! each host composes a different state. The backend selector and session
//! store are shared so real and simulated links use one path.

pub mod ble;
mod btleplug;
pub mod session;
