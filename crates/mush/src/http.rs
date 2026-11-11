//! Host side of soil:http for mush.
//!
//! The guest names a URL; the host owns the origin allowlist, TLS, body caps and
//! the transport. This is the same contract the `soil:http-host` runner exposes,
//! reached through `HttpHost::perform` so there is one copy of the policy rather
//! than a second one tuned for this shell.
//!
//! The allowlist is `MUSH_HTTP_ALLOW`, a comma-separated list of origins
//! (`https://api.github.com`). Empty means "nothing": a shell that can fetch
//! anything is ambient authority with extra steps.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::Result;
use soil_http_host::{CredentialProvider, Error, HttpHost, HttpPolicy, Method, Request};

use crate::soil::http::http;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The principal's groups decide the origin allowlist; see `access`.
pub fn host(access: &crate::access::Access) -> HttpHost {
    let origins = access.origins.iter().cloned().collect::<Vec<_>>();
    let policy = HttpPolicy::new(origins, MAX_BYTES, TIMEOUT).unwrap_or_else(|error| {
        // A malformed allowlist must not hand the caller a policy it cannot see.
        eprintln!("mush: unusable fetch policy ({error}); fetching nothing");
        HttpPolicy::new(Vec::new(), MAX_BYTES, TIMEOUT).expect("an empty policy is valid")
    });
    HttpHost::new(policy).with_credentials(Arc::new(EnvCredentials::from_env()))
}

/// Bearer tokens the host holds on the guest's behalf.
///
/// The guest only ever names a handle; the token is read from the host's own
/// environment (`MUSH_HTTP_TOKEN_<HANDLE>`) and injected here. A Geode adapter
/// implementing the same trait replaces this without the Spore changing.
#[derive(Default)]
pub struct EnvCredentials {
    tokens: BTreeMap<String, String>,
}

impl EnvCredentials {
    pub fn from_env() -> Self {
        let mut tokens = BTreeMap::new();
        for (name, value) in std::env::vars() {
            if let Some(handle) = name.strip_prefix("MUSH_HTTP_TOKEN_")
                && !handle.is_empty()
                && !value.is_empty()
            {
                tokens.insert(handle.to_ascii_lowercase(), value);
            }
        }
        Self { tokens }
    }
}

impl CredentialProvider for EnvCredentials {
    fn resolve(&self, handle: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .tokens
            .get(&handle.to_ascii_lowercase())
            .map(|token| ("authorization".to_string(), format!("Bearer {token}"))))
    }
}

impl http::Host for crate::Host {
    fn fetch(&mut self, request: http::Request) -> Result<http::Response, http::Error> {
        let response = self
            .http
            .perform(Request {
                url: request.url,
                method: method_from_wit(request.method),
                headers: request
                    .headers
                    .into_iter()
                    .map(|header| (header.name, header.value))
                    .collect(),
                body: request.body,
                credential: request.credential,
                max_bytes: request.max_bytes,
                timeout_ms: request.timeout_ms,
            })
            .map_err(error_to_wit)?;
        Ok(http::Response {
            status: response.status,
            final_url: response.final_url,
            headers: response
                .headers
                .into_iter()
                .map(|(name, value)| http::Header { name, value })
                .collect(),
            body: response.body,
        })
    }
}

fn method_from_wit(method: http::Method) -> Method {
    match method {
        http::Method::Get => Method::Get,
        http::Method::Head => Method::Head,
        http::Method::Post => Method::Post,
        http::Method::Put => Method::Put,
        http::Method::Patch => Method::Patch,
        http::Method::Delete => Method::Delete,
    }
}

fn error_to_wit(error: Error) -> http::Error {
    match error {
        Error::Denied => http::Error::Denied,
        Error::Invalid => http::Error::Invalid,
        Error::OverBudget => http::Error::OverBudget,
        Error::Timeout => http::Error::Timeout,
        Error::Unavailable => http::Error::Unavailable,
        Error::Tls => http::Error::Tls,
        Error::Http => http::Error::Http,
        Error::Io => http::Error::Io,
        Error::Unsupported => http::Error::Unsupported,
    }
}
