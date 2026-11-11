//! The wire contract.
//!
//! Two frames, one version:
//!
//! ```text
//! {"v":1,"call":{"id":"7","interface":"soil:records","function":"scan","params":{...}}}
//! {"v":1,"id":"7","result":{"ok":{...}}}
//! {"v":1,"id":"7","result":{"err":{"code":"denied","message":"..."}}}
//! {"v":1,"event":{"kind":"changed","collection":"worklog.event","id":"a","revision":9}}
//! ```
//!
//! Field names are camelCase on the wire and kebab-case in WIT. That is a
//! deliberate, mechanical mapping — `updated-at-ms` becomes `updatedAtMs` — and
//! it is the one a generated TypeScript client and an OpenAPI document both want.
//! Keeping kebab on the wire would make every JS access a bracket lookup for no
//! gain in fidelity, because the encoding already differs from the canonical
//! form in the one way that matters: a `resource` is an id here, not a handle.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_INTERFACE_BYTES: usize = 96;
pub const MAX_FUNCTION_BYTES: usize = 64;

/// One call, as received.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub v: u32,
    pub call: Call,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    /// Client-chosen correlation id, echoed on the result. Not an authority:
    /// two calls with the same id are two calls.
    pub id: String,
    pub interface: String,
    pub function: String,
    #[serde(default)]
    pub params: Value,
}

/// One result, as sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub v: u32,
    pub id: String,
    pub result: Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Value),
    Err(CallError),
}

/// A WIT `error` value, not an HTTP status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallError {
    pub code: Code,
    pub message: String,
}

/// The union of the `error` enums in the canonical WIT.
///
/// Shared rather than per-package because a browser only needs to branch once:
/// `denied` means ask the Spore author for a grant, `unsupported` means this
/// host cannot serve the interface at all, and everything else is the call's
/// own problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Code {
    Denied,
    NotFound,
    OverBudget,
    Invalid,
    Stale,
    Io,
    Unsupported,
}

impl CallError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(Code::Denied, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Code::Invalid, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(Code::Unsupported, message)
    }

    pub fn over_budget(message: impl Into<String>) -> Self {
        Self::new(Code::OverBudget, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(Code::Io, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Code::NotFound, message)
    }
}

/// Something the host wants every session to know.
///
/// `ready` carries the package grants and, per interface, what this instance is
/// bound to (`{"book":"worklog","collection":"worklog.event"}`). A client reads
/// its collection from here instead of hard-coding a second copy of host
/// configuration, and a page whose grants are empty sees that before it asks.
///
/// `changed` carries the collection, the id and the new revision so a client can
/// re-read exactly one record instead of re-scanning. It is a notification, not
/// the record itself: a client that missed events (a sleeping tab, a dropped
/// socket) re-scans on reconnect and is correct, which is why the bus may drop
/// an event for a slow subscriber.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Ready {
        spore: String,
        grants: Vec<String>,
        interfaces: BTreeMap<String, Value>,
    },
    Changed {
        collection: String,
        id: String,
        revision: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFrame {
    pub v: u32,
    pub event: Event,
}

impl Request {
    pub fn validate(&self) -> Result<(), CallError> {
        if self.v != VERSION {
            return Err(CallError::invalid(format!(
                "protocol version {} is not {VERSION}",
                self.v
            )));
        }
        self.call.validate()
    }
}

impl Call {
    pub fn validate(&self) -> Result<(), CallError> {
        if !ident(&self.id, MAX_ID_BYTES) {
            return Err(CallError::invalid(
                "call id must be 1-128 bytes of [a-z0-9._:-]",
            ));
        }
        if !ident(&self.interface, MAX_INTERFACE_BYTES) || !self.interface.contains(':') {
            return Err(CallError::invalid(
                "interface must be a WIT package name such as soil:records",
            ));
        }
        if !self
            .function
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || self.function.is_empty()
            || self.function.len() > MAX_FUNCTION_BYTES
        {
            return Err(CallError::invalid(
                "function must be a kebab-case WIT function name",
            ));
        }
        Ok(())
    }
}

fn ident(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._:-".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(interface: &str, function: &str) -> Call {
        Call {
            id: "1".into(),
            interface: interface.into(),
            function: function.into(),
            params: Value::Null,
        }
    }

    #[test]
    fn a_package_call_validates() {
        call("soil:records", "scan").validate().unwrap();
    }

    #[test]
    fn version_and_names_are_refused_rather_than_guessed() {
        let request = Request {
            v: 2,
            call: call("soil:records", "scan"),
        };
        assert_eq!(request.validate().unwrap_err().code, Code::Invalid);

        assert!(call("records", "scan").validate().is_err());
        assert!(call("soil:records", "SCAN").validate().is_err());
        assert!(call("soil:records", "").validate().is_err());
    }

    #[test]
    fn an_error_serializes_as_a_wit_error_and_not_a_status() {
        let response = Response {
            v: VERSION,
            id: "7".into(),
            result: Outcome::Err(CallError::denied("no soil:records.write")),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["result"]["err"]["code"], "denied");
        assert_eq!(json["v"], 1);
    }

    #[test]
    fn events_are_tagged_by_kind() {
        let frame = EventFrame {
            v: VERSION,
            event: Event::Changed {
                collection: "worklog.event".into(),
                id: "a".into(),
                revision: 9,
            },
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["event"]["kind"], "changed");
        assert_eq!(json["event"]["revision"], 9);
    }
}
