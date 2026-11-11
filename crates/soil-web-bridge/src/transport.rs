//! A `soil:http` transport answered by a session.
//!
//! This is the direction a Spore needs when the node cannot reach the network
//! but the page can: the host's `soil:http` import resolves to a fetch running
//! in the browser, with the browser's cookies, origin policy and CORS rules.
//!
//! Two properties are load-bearing:
//!
//! - **Absence is `Unsupported`, never a silent empty answer.** With no live
//!   provider the transport says it cannot serve the request, which is what lets
//!   [`PreferProvider`] fall back to the node's own network.
//! - **A refusal is an answer.** If the provider declines a URL, that reaches the
//!   Spore as `denied`; routing around it would quietly overrule the page's own
//!   policy.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use soil_host::http_host::{HttpTransport, TransportError, TransportRequest, TransportResponse};

use crate::protocol::Code;
use crate::server::Bridge;

pub struct SessionTransport {
    bridge: Arc<Bridge>,
}

impl SessionTransport {
    pub fn new(bridge: Arc<Bridge>) -> Self {
        Self { bridge }
    }
}

impl HttpTransport for SessionTransport {
    fn fetch(&self, request: &TransportRequest) -> Result<TransportResponse, TransportError> {
        if !self.bridge.provides("soil:http") {
            return Err(TransportError::Unsupported);
        }

        let params = json!({
            "url": request.url.as_str(),
            "method": request.method_name(),
            "headers": request
                .headers
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect::<Vec<_>>(),
            "body": request.body.as_ref().map(|body| STANDARD.encode(body)),
            "maxBytes": request.max_bytes,
            "timeoutMs": u32::try_from(request.timeout.as_millis()).unwrap_or(u32::MAX),
        });

        let value = self
            .bridge
            .call_provider("soil:http", "fetch", params, request.timeout)
            .map_err(|error| match error.code {
                Code::Denied => TransportError::Denied,
                Code::OverBudget => TransportError::OverBudget,
                Code::Timeout => TransportError::Timeout,
                Code::Unavailable => TransportError::Unavailable,
                Code::Unsupported => TransportError::Unsupported,
                _ => TransportError::Http,
            })?;

        let status = value
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .ok_or(TransportError::Http)?;
        let final_url = value
            .get("finalUrl")
            .and_then(Value::as_str)
            .ok_or(TransportError::Http)?
            .to_owned();
        let headers = value
            .get("headers")
            .and_then(Value::as_array)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|header| {
                        Some((
                            header.get("name")?.as_str()?.to_owned(),
                            header.get("value")?.as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let body = match value.get("body").and_then(Value::as_str) {
            Some(body) => STANDARD.decode(body).map_err(|_| TransportError::Http)?,
            None => Vec::new(),
        };
        if body.len() > request.max_bytes {
            return Err(TransportError::OverBudget);
        }

        Ok(TransportResponse {
            status,
            final_url,
            headers,
            body,
        })
    }
}
