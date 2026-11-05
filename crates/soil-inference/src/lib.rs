//! Shared Soil inference and credential configuration.
//!
//! `mush` uses this to configure Operator; `soil-inference-host` uses the same
//! types for the Spore-facing `soil:inference` contract.

mod config;
mod geode;

pub use config::{
    GeodeConfig, InferenceConfig, InferenceProfile, McpConfig, OperatorConfig, ResolvedOperator,
    SoilConfig,
};
pub use geode::GeodeResolver;
