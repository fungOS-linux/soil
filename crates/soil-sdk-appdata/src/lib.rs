//! Guest SDK for `soil:appdata`.
//!
//! App identity is derived by the host from the signed Spore manifest. The
//! Spore never supplies an app id or a host path.
//!
//! ```ignore
//! use soil_sdk_appdata::synced::Json;
//!
//! let settings = Json::<Settings>::open("settings")?;
//! settings.save(&Settings::default())?;
//! ```

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
    generate_all,
});

use std::marker::PhantomData;

use soil::appdata::appdata::{self, Meta, Scope, Store, SyncState};
use soil_sdk_codec::{Codec, JsonCodec};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("appdata access denied")]
    Denied,
    #[error("key not found")]
    NotFound,
    #[error("appdata budget exceeded")]
    OverBudget,
    #[error("invalid appdata key or generation")]
    Invalid,
    #[error("appdata generation conflict")]
    Conflict,
    #[error("synced appdata is not ready")]
    NotReady,
    #[error("appdata store failed")]
    Io,
    #[error(transparent)]
    Codec(#[from] soil_sdk_codec::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<appdata::Error> for Error {
    fn from(error: appdata::Error) -> Self {
        match error {
            appdata::Error::Denied => Self::Denied,
            appdata::Error::NotFound => Self::NotFound,
            appdata::Error::OverBudget => Self::OverBudget,
            appdata::Error::Invalid => Self::Invalid,
            appdata::Error::Conflict => Self::Conflict,
            appdata::Error::NotReady => Self::NotReady,
            appdata::Error::Io => Self::Io,
        }
    }
}

/// One typed AppData value backed by a JSON key.
pub struct Json<T> {
    store: Store,
    key: String,
    codec: JsonCodec,
    _value: PhantomData<T>,
}

impl<T> Json<T> {
    pub fn open(scope: Scope, key: impl Into<String>) -> Result<Self> {
        let store = appdata::open(scope)?;
        Ok(Self {
            store,
            key: key.into(),
            codec: JsonCodec,
            _value: PhantomData,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn generation(&self) -> Result<u64> {
        self.store.generation().map_err(Error::from)
    }

    pub fn sync_state(&self) -> Result<SyncState> {
        self.store.sync_state().map_err(Error::from)
    }

    pub fn delete(&self) -> Result<()> {
        self.store.delete(&self.key).map_err(Error::from)
    }

    pub fn list_keys(&self, prefix: impl AsRef<str>, limit: u32) -> Result<Vec<Meta>> {
        self.store
            .list_keys(prefix.as_ref(), limit)
            .map_err(Error::from)
    }
}

impl<T: serde::Serialize + serde::de::DeserializeOwned> Json<T> {
    pub fn load(&self) -> Result<Option<T>> {
        let Some(bytes) = self.store.get(&self.key).map_err(Error::from)? else {
            return Ok(None);
        };
        Ok(Some(self.codec.decode(&bytes)?))
    }

    pub fn load_or_default(&self) -> Result<T>
    where
        T: Default,
    {
        Ok(self.load()?.unwrap_or_default())
    }

    pub fn load_with_meta(&self) -> Result<Option<(T, Meta)>> {
        let Some(meta) = self.store.get_meta(&self.key).map_err(Error::from)? else {
            return Ok(None);
        };
        let Some(bytes) = self.store.get(&self.key).map_err(Error::from)? else {
            return Ok(None);
        };
        Ok(Some((self.codec.decode(&bytes)?, meta)))
    }

    pub fn save(&self, value: &T) -> Result<u64> {
        let bytes = self.codec.encode(value)?;
        self.store.set(&self.key, &bytes).map_err(Error::from)
    }

    /// Compare-and-set against the generation returned by `load_with_meta`.
    pub fn save_if_generation(&self, value: &T, generation: u64) -> Result<u64> {
        let bytes = self.codec.encode(value)?;
        self.store
            .set_if_generation(&self.key, &bytes, generation)
            .map_err(Error::from)
    }
}

pub mod local {
    use super::*;

    /// Device-only AppData.
    pub struct Json<T>(super::Json<T>);

    impl<T> Json<T> {
        pub fn open(key: impl Into<String>) -> Result<Self> {
            Ok(Self(super::Json::<T>::open(Scope::Local, key)?))
        }
    }

    impl<T> std::ops::Deref for Json<T> {
        type Target = super::Json<T>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
}

pub mod synced {
    use super::*;

    /// Per-user, per-app AppData replicated through the native records engine.
    pub struct Json<T>(super::Json<T>);

    impl<T> Json<T> {
        pub fn open(key: impl Into<String>) -> Result<Self> {
            Ok(Self(super::Json::<T>::open(Scope::Synced, key)?))
        }
    }

    impl<T> std::ops::Deref for Json<T> {
        type Target = super::Json<T>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
}

#[cfg(test)]
mod tests {
    const WIT: &str = include_str!("../../../wit/soil-appdata.wit");

    #[test]
    fn pins_the_guest_world() {
        assert!(WIT.contains("package soil:appdata@1.0.0;"));
        assert!(WIT.contains("world guest"));
        assert!(WIT.contains("resource store"));
        assert!(WIT.contains("set-if-generation"));
    }
}
