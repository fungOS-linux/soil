//! Guest-side value codecs for Soil storage.
//!
//! The WIT boundary is bytes. This crate is a convenience layer; a Spore can
//! always supply its own encoding.

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("encode value: {0}")]
    Encode(serde_json::Error),
    #[error("decode value: {0}")]
    Decode(serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Encode and decode one typed value.
pub trait Codec {
    fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>>;
    fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T>;
}

/// Human-debuggable JSON. This is the default AppData codec.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonCodec;

impl Codec for JsonCodec {
    fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>> {
        serde_json::to_vec(value).map_err(Error::Encode)
    }

    fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T> {
        serde_json::from_slice(bytes).map_err(Error::Decode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Prefs {
        theme: String,
        rows: u32,
    }

    #[test]
    fn json_round_trip() {
        let codec = JsonCodec;
        let value = Prefs {
            theme: "dark".into(),
            rows: 42,
        };
        let bytes = codec.encode(&value).unwrap();
        assert_eq!(codec.decode::<Prefs>(&bytes).unwrap(), value);
    }
}
