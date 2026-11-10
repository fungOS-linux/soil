//! Soil guest SDK.
//!
//! This is an umbrella over capability-specific guest crates. It deliberately
//! does not generate one giant world: a Spore imports only the feature crates
//! it actually uses, so the linker still sees a narrow capability set.

#[cfg(feature = "audio")]
pub use soil_sdk_audio as audio;

#[cfg(feature = "appdata")]
pub use soil_sdk_appdata as appdata;

#[cfg(feature = "canvas")]
pub use soil_sdk_canvas as canvas;

#[cfg(feature = "appdata")]
pub use soil_sdk_appdata::Result as AppDataResult;

#[cfg(feature = "records")]
pub use soil_sdk_records as records;

#[cfg(feature = "unibus")]
pub use soil_sdk_unibus as unibus;
