//! The one host: every `soil:*` capability a Spore can be given, and the WASI
//! p2 profile it runs inside.
//!
//! It was two crates — a capability crate and a runner that depended on it —
//! and three before that, one per capability. That split bought a `Cargo.toml`
//! per contract, a duplicated error type, and an import graph nobody could hold
//! in their head, while every consumer that wanted a host wanted all of it. A
//! host is one thing, so it is one crate.
//!
//! Capabilities, one module per contract:
//!
//! * `vfs_host` serves `soil:vfs`, `soil:vfs-mut` and `soil:index` over
//!   `fungos-vfs`;
//! * `http_host` serves `soil:http`;
//! * `records_host` serves `soil:records`;
//! * `principal_host` serves `soil:principal`;
//! * `unibus` serves `soil:unibus`;
//! * `creds` serves `soil:creds`.
//!
//! The profile around them:
//!
//! * `wasi:cli`, `wasi:clocks`, `wasi:random`, `wasi:filesystem`, `wasi:sockets`
//!   through `wasmtime-wasi`;
//! * `wasi:http` incoming and outgoing through `wasmtime-wasi-http`;
//! * `fungos:http-context` for host-attested invocation facts;
//! * Soil-owned policy around origins, CIDRs, DNS, preopens and field limits;
//! * [`HttpGateway`] for vhost, route, method, body and deadline grants;
//! * [`WasiHost`], which links all of it and hands a guest authority only where
//!   the policy allows the specific operation.
//!
//! Secrets, the Unibus session, sockets and TLS stay in the host.
//!
//! # Features
//!
//! Every module is behind the feature that names it, so a consumer links the
//! capabilities it will actually grant instead of the whole host: a Spore
//! runner that shows documents wants `vfs`, `records` and `principal`; a window
//! that grants `soil:http` wants `http`. `wasi` is the runner profile — the
//! WASI p2 imports, the credential and bus sessions, the gateway and
//! [`WasiHost`] that links them — and it is the one feature that pulls the
//! others in, because those modules are written against it. `cli` adds the
//! `soil-*` commands.

#[cfg(feature = "wasi")]
pub mod context;
#[cfg(feature = "wasi")]
pub mod creds;
#[cfg(feature = "wasi")]
mod gateway;
#[cfg(feature = "wasi")]
mod host;
#[cfg(any(feature = "http", feature = "wasi"))]
pub mod http_host;
#[cfg(any(feature = "http", feature = "wasi"))]
mod policy;
#[cfg(feature = "principal")]
pub mod principal_host;
#[cfg(feature = "records")]
pub mod records_host;
#[cfg(feature = "wasi")]
mod service;
#[cfg(feature = "wasi")]
pub mod time;
#[cfg(feature = "wasi")]
pub mod unibus;
#[cfg(feature = "vfs")]
pub mod vfs_host;

#[cfg(feature = "wasi")]
pub use context::HttpContext;
#[cfg(feature = "wasi")]
pub use creds::{
    Algorithm, CredentialError, CredentialProvider, GeodeCredentials, GeodePgpCredentials,
    ResolvedHandle, StaticCredentials,
};
#[cfg(feature = "wasi")]
pub use gateway::{
    GatewayRoute, GatewayVhost, HttpGateway, HttpGatewayBuilder, IngressError, OriginAuth,
};
#[cfg(feature = "wasi")]
pub use host::{HttpCapability, Preopen, WasiHost, WasiHostBuilder};
#[cfg(any(feature = "http", feature = "wasi"))]
pub use http_host::HttpHost;
#[cfg(any(feature = "http", feature = "wasi"))]
pub use policy::{HttpPolicy, NetworkPolicy, WasiHostPolicy};
#[cfg(feature = "records")]
pub use records_host::RecordsBookHost;
#[cfg(feature = "wasi")]
pub use service::HttpService;
#[cfg(feature = "wasi")]
pub use unibus::{
    Bus, BusError, BusReceipt, Frame, MAX_BUS_BODY, MAX_POLL_LIMIT, MemoryBus, Page, UnibusBus,
};
#[cfg(feature = "vfs")]
pub use vfs_host::VfsHost;

/// The set of host facilities currently provided by this profile.
#[cfg(feature = "wasi")]
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
