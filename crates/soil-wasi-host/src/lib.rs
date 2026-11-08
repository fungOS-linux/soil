//! Reusable WASI p2 host profile for Soil Spores.
//!
//! The profile is intentionally small:
//!
//! * `wasi:cli`, `wasi:clocks`, `wasi:random`, `wasi:filesystem`, `wasi:sockets`
//!   through `wasmtime-wasi`;
//! * `wasi:http` incoming and outgoing through `wasmtime-wasi-http`;
//! * `fungos:http-context` for host-attested invocation facts;
//! * `soil:creds` for host-held signing and verification;
//! * `soil:unibus` for bounded poll/publish over the bus;
//! * Soil-owned policy around origins, CIDRs, DNS, preopens and field limits;
//! * [`HttpGateway`] for vhost, route, method, body and deadline grants.
//!
//! A Spore gets the WASI interface because it was linked. It gets authority only
//! because the host policy allows the specific operation. Secrets, the Unibus
//! session, sockets and TLS stay in the host.

pub mod context;
pub mod creds;
mod gateway;
mod host;
mod policy;
mod service;
pub mod unibus;

pub use context::HttpContext;
pub use creds::{
    Algorithm, CredentialError, CredentialProvider, ResolvedHandle, StaticCredentials,
};
pub use gateway::{
    GatewayRoute, GatewayVhost, HttpGateway, HttpGatewayBuilder, IngressError, OriginAuth,
};
pub use host::{Preopen, WasiHost, WasiHostBuilder};
pub use policy::{HttpPolicy, NetworkPolicy, WasiHostPolicy};
pub use service::HttpService;
pub use unibus::{
    Bus, BusError, BusReceipt, Frame, MAX_BUS_BODY, MAX_POLL_LIMIT, MemoryBus, Page, UnibusBus,
};

/// The set of host facilities currently provided by this profile.
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
    "fungos:http-context",
    "soil:creds",
    "soil:unibus",
];
