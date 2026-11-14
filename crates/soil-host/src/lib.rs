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

pub mod context;
pub mod creds;
mod gateway;
pub mod http_host;
mod host;
mod policy;
pub mod records_host;
mod service;
pub mod unibus;
pub mod vfs_host;

pub use context::HttpContext;
pub use creds::{
    Algorithm, CredentialError, CredentialProvider, GeodeCredentials, GeodePgpCredentials,
    ResolvedHandle, StaticCredentials,
};
pub use gateway::{
    GatewayRoute, GatewayVhost, HttpGateway, HttpGatewayBuilder, IngressError, OriginAuth,
};
pub use host::{HttpCapability, Preopen, WasiHost, WasiHostBuilder};
pub use http_host::HttpHost;
pub use policy::{HttpPolicy, NetworkPolicy, WasiHostPolicy};
pub use records_host::RecordsBookHost;
pub use service::HttpService;
pub use unibus::{
    Bus, BusError, BusReceipt, Frame, MAX_BUS_BODY, MAX_POLL_LIMIT, MemoryBus, Page, UnibusBus,
};
pub use vfs_host::VfsHost;

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
