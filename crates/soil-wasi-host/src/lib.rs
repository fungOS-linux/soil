//! Reusable WASI p2 host profile for Soil Spores.
//!
//! The profile is intentionally small:
//!
//! * `wasi:cli`, `wasi:clocks`, `wasi:random`, `wasi:filesystem`, `wasi:sockets`
//!   through `wasmtime-wasi`;
//! * `wasi:http` incoming and outgoing through `wasmtime-wasi-http`;
//! * Soil-owned policy around origins, CIDRs, DNS, preopens and field limits.
//!
//! A Spore gets the WASI interface because it was linked. It gets authority only
//! because the host policy allows the specific operation.

mod host;
mod policy;
mod service;

pub use host::{Preopen, WasiHost, WasiHostBuilder};
pub use policy::{HttpPolicy, NetworkPolicy, WasiHostPolicy};
pub use service::HttpService;

/// The set of WASI facilities currently provided by this profile.
pub const WASI_PROFILE: &[&str] = &[
    "wasi:cli",
    "wasi:clocks",
    "wasi:filesystem",
    "wasi:http/incoming-handler",
    "wasi:http/outgoing-handler",
    "wasi:http/types",
    "wasi:io",
    "wasi:random",
    "wasi:sockets",
];
