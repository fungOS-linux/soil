//! Host implementations of the `soil:*` capability contracts.
//!
//! One module per contract: `vfs_host` serves `soil:vfs`, `soil:vfs-mut` and
//! `soil:index` over `fungos-vfs`; `http_host` serves `soil:http`;
//! `records_host` serves `soil:records`.
//!
//! They live in one crate because they are one host. A binary that links the
//! shell links all three, and a crate per contract only bought a `Cargo.toml`,
//! a duplicated error type and a binary per experiment — the leaf-crate shape
//! this replaces.

pub mod http_host;
pub mod records_host;
pub mod vfs_host;

pub use http_host::HttpHost;
pub use records_host::RecordsBookHost;
pub use vfs_host::VfsHost;
