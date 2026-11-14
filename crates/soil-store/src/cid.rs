//! The content address a package needs, re-exported from `soil-cid`.
//!
//! There is one implementation of CIDv1-raw-sha256 in Soil: `soil-cid`. The
//! store keeps this module as its public path (`soil_store::cid::raw`) so the
//! publisher and the cache cannot drift apart.

pub use soil_cid::*;
