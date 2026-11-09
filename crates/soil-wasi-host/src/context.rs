//! `fungos:http-context`: host-attested facts about one HTTP invocation.
//!
//! The contract is an import, not a header convention. A Spore cannot set its
//! own identity, route or peer, and a client cannot spoof them either.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use wasmtime::component::{HasSelf, Linker};

use crate::host::WasiHost;

wasmtime::component::bindgen!({
    path: "../../wit/fungos-http-context.wit",
    world: "host",
});

use fungos::http_context::context as api;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Host-owned context for one `wasi:http/incoming-handler` call.
///
/// `route` is the matched host grant, not the request path, so it cannot be
/// influenced by the caller. `peer` is the transport peer the host accepted;
/// `tls_peer` stays `None` until the listener terminates TLS and verifies a
/// client certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpContext {
    pub request_id: String,
    pub spore: String,
    pub route: String,
    pub peer: Option<String>,
    pub tls_peer: Option<String>,
    pub received_at_ms: u64,
}

impl HttpContext {
    /// Build a context for one accepted request.
    pub fn new(
        spore: impl Into<String>,
        route: impl Into<String>,
        peer: Option<String>,
        tls_peer: Option<String>,
    ) -> Self {
        Self {
            request_id: new_request_id(),
            spore: spore.into(),
            route: route.into(),
            peer,
            tls_peer,
            received_at_ms: now_ms(),
        }
    }
}

impl api::Host for WasiHost {
    fn current(&mut self) -> api::RequestContext {
        let context = self.http_context().unwrap_or_else(|| {
            panic!(
                "fungos:http-context.current() called outside a host HTTP invocation; \
                 the host has no request context to report"
            )
        });
        api::RequestContext {
            request_id: context.request_id.clone(),
            spore: context.spore.clone(),
            route: context.route.clone(),
            peer: context.peer.clone(),
            tls_peer: context.tls_peer.clone(),
            received_at_ms: context.received_at_ms,
        }
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

fn new_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:032x}{sequence:016x}")
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
