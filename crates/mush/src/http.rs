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

use std::time::Duration;

use soil_http_host::{Error, HttpPolicy, Method, Request};

use crate::soil::http::http;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub fn policy_from_env() -> HttpPolicy {
    let configured = std::env::var("MUSH_HTTP_ALLOW").unwrap_or_default();
    let origins = configured
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    HttpPolicy::new(origins, MAX_BYTES, TIMEOUT).unwrap_or_else(|error| {
        // A malformed allowlist must not hand the caller a policy it cannot see.
        eprintln!("mush: MUSH_HTTP_ALLOW ignored: {error}");
        HttpPolicy::new(Vec::new(), MAX_BYTES, TIMEOUT).expect("an empty policy is valid")
    })
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
