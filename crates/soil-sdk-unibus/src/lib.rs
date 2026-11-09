//! Guest SDK for `soil:unibus`.
//!
//! The host owns the Unibus session, the credential, hub selection and the
//! retention window. A Spore names the kinds it observes, gets opaque body
//! bytes back, and never sees a socket, peer identity, or token.
//!
//! The contract is cursor-paged ([`poll`] takes `cursor`/`limit` and returns
//! `next`) because a Spore may be suspended and resumed between two pages. A
//! retained page that aged out of the host's window is [`Error::Stale`], never
//! a silent gap: the caller decides whether to restart at the live tail.

wit_bindgen::generate!({
    path: "wit",
    world: "unibus-host",
    generate_all,
});

use serde::{Serialize, de::DeserializeOwned};
use soil::unibus::bus::{self, Frame, Page};
use thiserror::Error;

/// Largest body the host will accept on either side of the boundary.
///
/// Matches `soil_wasi_host::unibus::MAX_BUS_BODY`, which in turn matches the
/// Unibus control-plane ceiling. Bulk payloads belong on the content plane as
/// a reference (CID/URL) inside the JSON object, not as bytes here.
pub const MAX_BODY: usize = 64 * 1024;

/// Longest `kind` the host forwards to the transport.
pub const MAX_KIND_BYTES: usize = 128;

/// Largest page the host will serve in one call.
pub const MAX_POLL_LIMIT: u32 = 256;

#[derive(Debug, Error)]
pub enum Error {
    #[error("unibus access denied")]
    Denied,
    #[error("invalid kind, cursor, or body")]
    Invalid,
    #[error("unibus page or body exceeds the budget")]
    OverBudget,
    #[error("unibus cursor predates the retention window")]
    Stale,
    #[error("unibus is unreachable")]
    Unavailable,
    #[error("body must be a JSON object, got a {0}")]
    NotAnObject(&'static str),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<bus::Error> for Error {
    fn from(error: bus::Error) -> Self {
        match error {
            bus::Error::Denied => Self::Denied,
            bus::Error::Invalid => Self::Invalid,
            bus::Error::OverBudget => Self::OverBudget,
            bus::Error::Stale => Self::Stale,
            bus::Error::Unavailable => Self::Unavailable,
        }
    }
}

/// Poll retained frames for `kinds`.
///
/// An empty `kinds` list means no kinds, never "all": a Spore has to name what
/// it observes. Pass the returned `page.next` back as `cursor` for the next
/// page; a missing cursor starts at the live tail.
pub fn poll(kinds: &[&str], cursor: Option<&str>, limit: u32) -> Result<Page> {
    let kinds: Vec<String> = kinds.iter().map(|kind| (*kind).to_owned()).collect();
    Ok(bus::poll(&kinds, cursor, limit)?)
}

/// Publish one frame and return its request id.
///
/// The host requires a JSON-object body and a granted `soil:unibus.publish`
/// capability; `ttl_ms == 0` means the host default.
pub fn publish(
    kind: &str,
    body: impl AsRef<[u8]>,
    destination: Option<&str>,
    ttl_ms: u64,
) -> Result<String> {
    let destination = destination.map(str::to_owned);
    Ok(bus::publish(kind, body.as_ref(), destination.as_deref(), ttl_ms)?.request_id)
}

/// Encode `value` as the JSON object the host expects.
///
/// Split out of [`publish_json`] so the shape rule is testable without a host:
/// a scalar or array is a caller bug, not something to nest silently.
pub fn encode_object<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let text = serde_json::to_string(value)?;
    match text.trim_start().as_bytes().first() {
        Some(b'{') => Ok(text.into_bytes()),
        _ => Err(Error::NotAnObject(json_kind(&text))),
    }
}

fn json_kind(text: &str) -> &'static str {
    match text.trim_start().as_bytes().first() {
        Some(b'[') => "array",
        Some(b'"') => "string",
        Some(b't') | Some(b'f') => "boolean",
        Some(b'n') => "null",
        _ => "number",
    }
}

/// Encode `value` as a JSON object and publish it.
pub fn publish_json<T: Serialize + ?Sized>(
    kind: &str,
    value: &T,
    destination: Option<&str>,
    ttl_ms: u64,
) -> Result<String> {
    publish(kind, encode_object(value)?, destination, ttl_ms)
}

/// Decode a frame body produced by [`publish_json`] (or any JSON-object peer).
pub fn decode<T: DeserializeOwned>(frame: &Frame) -> Result<T> {
    Ok(serde_json::from_slice(&frame.body)?)
}

/// A cursor that advances with each page.
///
/// A worker loop is `loop { for frame in tail.next(MAX_POLL_LIMIT)? { … } }`.
/// On error the cursor stays where it was, so a `Stale` or `Unavailable` page is
/// retried or reset explicitly by the caller rather than skipped silently.
pub struct Tail {
    kinds: Vec<String>,
    cursor: Option<String>,
}

impl Tail {
    pub fn new(kinds: &[&str]) -> Self {
        Self {
            kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
            cursor: None,
        }
    }

    pub fn kinds(&self) -> &[String] {
        &self.kinds
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    /// Fetch the next page and advance the cursor past it.
    pub fn next(&mut self, limit: u32) -> Result<Vec<Frame>> {
        let page = bus::poll(&self.kinds, self.cursor.as_deref(), limit)?;
        self.cursor = page.next;
        Ok(page.frames)
    }

    /// Forget the cursor and restart at the live tail.
    pub fn reset(&mut self) {
        self.cursor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIT: &str = include_str!("../../../wit/soil-unibus.wit");

    #[test]
    fn pins_the_guest_world() {
        assert!(WIT.contains("package soil:unibus@0.1.0;"));
        assert!(WIT.contains("world unibus-host"));
        assert!(WIT.contains("poll: func("));
        assert!(WIT.contains("publish: func("));
        assert!(WIT.contains("stale,"));
    }

    #[test]
    fn body_must_be_an_object() {
        assert_eq!(
            encode_object(&[1, 2]).unwrap_err().to_string(),
            "body must be a JSON object, got a array"
        );
        assert_eq!(
            encode_object(&7).unwrap_err().to_string(),
            "body must be a JSON object, got a number"
        );
        assert_eq!(
            encode_object("hi").unwrap_err().to_string(),
            "body must be a JSON object, got a string"
        );
    }

    #[test]
    fn object_round_trips() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Hit {
            cue: String,
        }

        let body = encode_object(&Hit { cue: "door".into() }).unwrap();
        assert_eq!(body, br#"{"cue":"door"}"#.to_vec());
        let frame = Frame {
            kind: "sfx.cue.v1".into(),
            producer: "spore".into(),
            issued_at_ms: 0,
            body,
        };
        assert_eq!(decode::<Hit>(&frame).unwrap(), Hit { cue: "door".into() });
    }
}
