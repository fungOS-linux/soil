//! A browser is a second host.
//!
//! A Spore runs against the canonical WIT packages in [`wit/`](../../wit). On a
//! desktop the host implements those imports in-process; in a browser there is
//! no Soil, so the same imports have to arrive over the network. This crate is
//! that arrival: one HTTP request/response plane for unary calls and one
//! WebSocket for the duplex session, and nothing else.
//!
//! Three rules decide the shape of everything here.
//!
//! **The bridge is an adapter, not an ABI.** Every call names an interface and a
//! function from the canonical WIT (`soil:records` / `scan`), never a bespoke
//! browser route. A page that targets the bridge directly has forked the
//! contract; a page that targets the WIT is a host implementation away from
//! running natively.
//!
//! **Grants travel with the connection, not with the user.** A session is bound
//! to one Spore package and its `[grants]` table, read from `spore.toml`. A page
//! holding a valid token still cannot call an interface its Spore did not
//! declare, and the check happens before any host work. This is the same rule
//! the wasmtime host enforces at link time, moved to where the browser can see
//! it.
//!
//! **A WIT error is a value.** Capability failures come back as
//! `{"result":{"err":{"code":"denied"}}}` with HTTP 200, because they are
//! part of the interface. HTTP status codes are reserved for transport
//! failures: malformed JSON, an oversized body, a missing token.
//!
//! The envelope is direction-neutral. Today the node answers and the browser
//! calls. When the browser implements an interface the node needs — `soil:http`
//! over `fetch`, `soil:audio` over `getUserMedia` — the same `call`/`result`
//! frames travel the other way over the same session.

pub mod grants;
pub mod protocol;
pub mod records;
pub mod server;
pub mod transport;

pub use grants::GrantSet;
pub use protocol::{
    Call, CallError, Code, Event, Frame, Outcome, Provide, ProvideRequest, Request, Response,
    VERSION,
};
pub use records::RecordsCapability;
pub use server::{Auth, Bridge, Server, serve};
pub use transport::SessionTransport;

use std::collections::BTreeMap;

use serde_json::Value;

/// What one capability call produced: the value the client asked for, plus the
/// events every session should see as a result.
#[derive(Debug)]
pub struct CallOutput {
    pub value: Value,
    pub events: Vec<Event>,
}

impl CallOutput {
    pub fn value(value: Value) -> Self {
        Self {
            value,
            events: Vec::new(),
        }
    }

    pub fn with_event(value: Value, event: Event) -> Self {
        Self {
            value,
            events: vec![event],
        }
    }
}

/// One WIT package served over the bridge.
///
/// `grant` names the sub-capability a function requires (`soil:records.write`),
/// or `None` when the package grant is enough on its own. Returning the name
/// rather than a boolean keeps the refusal message honest about what was
/// missing.
pub trait Capability: Send + Sync {
    fn package(&self) -> &str;
    fn grant(&self, function: &str) -> Option<&'static str>;
    fn call(&self, function: &str, params: &Value) -> Result<CallOutput, CallError>;

    /// What this instance is bound to, for the `ready` event.
    ///
    /// Host configuration a client needs to name (a book, a collection) is
    /// published here rather than duplicated in a page. It is descriptive only:
    /// nothing a client sends can change it.
    fn describe(&self) -> Value {
        Value::Null
    }
}

/// The packages this host can serve.
#[derive(Default)]
pub struct Registry {
    packages: BTreeMap<String, Box<dyn Capability>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, capability: impl Capability + 'static) -> Self {
        self.packages
            .insert(capability.package().to_owned(), Box::new(capability));
        self
    }

    /// The packages this host serves, with what each is bound to.
    pub fn describe(&self) -> BTreeMap<String, Value> {
        self.packages
            .iter()
            .map(|(package, capability)| (package.clone(), capability.describe()))
            .collect()
    }

    /// Grant check first, then the call.
    ///
    /// Ordering is the whole confinement argument: an ungranted call does no
    /// work, touches no store and returns no data, so a misbehaving page learns
    /// only that it was refused.
    pub fn dispatch(&self, grants: &GrantSet, call: &Call) -> (Outcome, Vec<Event>) {
        let Some(capability) = self.packages.get(&call.interface) else {
            return (
                Outcome::Err(CallError::unsupported(format!(
                    "this host does not serve {}",
                    call.interface
                ))),
                Vec::new(),
            );
        };

        if let Some(required) = capability.grant(&call.function)
            && !grants.allows(capability.package(), required)
        {
            return (
                Outcome::Err(CallError::denied(format!(
                    "{} needs {} in [grants]",
                    call.interface,
                    grant_name(capability.package(), required)
                ))),
                Vec::new(),
            );
        }

        match capability.call(&call.function, &call.params) {
            Ok(output) => (Outcome::Ok(output.value), output.events),
            Err(error) => (Outcome::Err(error), Vec::new()),
        }
    }
}

/// The grant name a refusal should quote: `soil:records.write`.
pub fn grant_name(package: &str, capability: &str) -> String {
    format!("{package}.{capability}")
}
