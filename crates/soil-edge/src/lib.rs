//! Soil L4 edge.
//!
//! A provider's TLS connection arrives at one public IP. The edge reads the
//! ClientHello SNI, selects a granted backend, and copies the TLS byte stream.
//! It never terminates TLS, parses HTTP, or sees a webhook body. Soil
//! terminates TLS at the backend and performs provider authentication there.

pub mod client_hello;
pub mod edge;
pub mod route;

pub use client_hello::{HelloError, HelloParse, MAX_CLIENT_HELLO, parse_server_name};
pub use edge::{Edge, EdgeConfig};
pub use route::{Backend, Routes};
