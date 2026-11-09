//! Confluence webhook handler on the Soil HTTP surface.
//!
//! This is the first real caller of `fungos:http-context`, `soil:creds`, and
//! `soil:unibus`. It verifies a Standard Webhooks signature without ever
//! seeing the provider secret, normalizes the delivery, and publishes one
//! durable fact. The host owns TLS, routing, the Unibus session, and the
//! tenant stamp.

use std::collections::BTreeMap;
use std::io::Write as _;

use base64::Engine as _;
use wasip2::http::types::{
    Headers, IncomingBody, IncomingRequest, Method, OutgoingBody, OutgoingResponse,
    ResponseOutparam,
};
use wasip2::io::streams::StreamError;

mod capabilities {
    wit_bindgen::generate!({
        path: "wit",
        world: "confluence",
        with: {
            "fungos:http-context/context@0.1.0": generate,
            "soil:creds/creds@1.0.0": generate,
            "soil:unibus/bus@0.1.0": generate,
        },
    });
}

use capabilities::fungos::http_context::context;
use capabilities::soil::creds::creds;
use capabilities::soil::unibus::bus;

const EVENT_KIND: &str = "confluence.webhook.received.v1";
const CREDENTIAL: &str = "confluence.webhook";
const MAX_BODY: usize = 64 * 1024;
const MAX_SKEW_SECONDS: i64 = 300;

struct Confluence;

impl wasip2::exports::http::incoming_handler::Guest for Confluence {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let context = context::current();
        let status = receive(&request, &context, response_out);
        let _ = status;
    }
}

/// Return the HTTP status so the request path is testable without a socket.
fn receive(
    request: &IncomingRequest,
    context: &context::RequestContext,
    response_out: ResponseOutparam,
) -> u16 {
    if !matches!(request.method(), Method::Post) {
        return reply(response_out, 405, json_error(405), "POST");
    }

    let path = request.path_with_query().unwrap_or_default();
    let path = path.split('?').next().unwrap_or_default();
    // `/webhooks/<provider>` uses the deployment's single secret;
    // `/webhooks/<provider>/<slug>` names one registered hook, and the slug is
    // the credential name the host resolves. The path is the only thing that
    // selects a secret, so adding a hook needs no restart and no new route.
    let Some((provider, slug)) = path
        .strip_prefix("/webhooks/")
        .and_then(split_hook_path)
    else {
        return reply(response_out, 404, json_error(404), "");
    };

    let headers = request.headers();
    let Some(webhook_id) = header(&headers, "webhook-id") else {
        return reply(response_out, 401, json_error(401), "");
    };
    let Some(timestamp) = header(&headers, "webhook-timestamp") else {
        return reply(response_out, 401, json_error(401), "");
    };
    let Some(signature_header) = header(&headers, "webhook-signature") else {
        return reply(response_out, 401, json_error(401), "");
    };
    let Ok(timestamp_seconds) = timestamp.parse::<i64>() else {
        return reply(response_out, 401, json_error(401), "");
    };
    let now_seconds = (context.received_at_ms / 1000) as i64;
    if (now_seconds - timestamp_seconds).abs() > MAX_SKEW_SECONDS {
        return reply(response_out, 401, json_error(401), "");
    }

    let body = match read_body(request) {
        Ok(body) => body,
        Err(status) => return reply(response_out, status, json_error(status), ""),
    };
    let Ok(value @ serde_json::Value::Object(_)) =
        serde_json::from_slice::<serde_json::Value>(&body)
    else {
        return reply(response_out, 415, json_error(415), "");
    };

    let signable = [
        webhook_id.as_bytes(),
        b".",
        timestamp.as_bytes(),
        b".",
        body.as_slice(),
    ]
    .concat();
    let Some(handle) = resolve_credential(slug) else {
        return reply(response_out, 503, json_error(503), "");
    };
    if !verify(&handle, &signable, &signature_header) {
        return reply(response_out, 401, json_error(401), "");
    }

    let event_type = header(&headers, "x-event-type").unwrap_or_else(|| "webhook".to_string());
    let event = serde_json::json!({
        "provider": provider,
        "delivery_id": webhook_id,
        "event_type": event_type,
        "received_at_ms": context.received_at_ms,
        "headers": safe_headers(&headers),
        "body": value,
    });
    let Ok(body) = serde_json::to_vec(&event) else {
        return reply(response_out, 500, json_error(500), "");
    };

    // `soil:unibus` has no idempotency key and no duplicate flag: the host
    // collapses redelivery on `provider` + `delivery_id` and hands the
    // original acceptance back. Both first and redelivery are therefore an
    // accepted 202; only the native (non-Spore) ingress distinguishes them.
    match bus::publish(EVENT_KIND, &body, None, 0) {
        Ok(receipt) => {
            let status = 202;
            let body = serde_json::json!({
                "status": status,
                "request_id": receipt.request_id,
                "duplicate": false,
            })
            .to_string();
            reply(response_out, status, body, "")
        }
        Err(_) => reply(response_out, 503, json_error(503), ""),
    }
}

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, u16> {
    let Ok(body) = request.consume() else {
        return Err(400);
    };
    let Ok(stream) = body.stream() else {
        return Err(400);
    };
    let mut data = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) => {
                if chunk.is_empty() {
                    break;
                }
                if data.len() + chunk.len() > MAX_BODY {
                    return Err(413);
                }
                data.extend_from_slice(&chunk);
            }
            Err(StreamError::Closed) => break,
            Err(_) => return Err(413),
        }
    }
    drop(stream);
    let _ = IncomingBody::finish(body);
    Ok(data)
}

/// `provider` or `provider/slug`, with no further segments.
fn split_hook_path(rest: &str) -> Option<(&str, &str)> {
    let mut segments = rest.split('/');
    let provider = segments.next().filter(|part| !part.is_empty())?;
    let slug = match segments.next() {
        Some(slug) if !slug.is_empty() => slug,
        Some(_) => return None,
        None => CREDENTIAL,
    };
    segments.next().is_none().then_some((provider, slug))
}

fn resolve_credential(name: &str) -> Option<creds::Handle> {
    creds::resolve(name).ok()
}

fn verify(handle: &creds::Handle, message: &[u8], header: &str) -> bool {
    for candidate in header.split_ascii_whitespace() {
        let Some(signature) = candidate.strip_prefix("v1,") else {
            continue;
        };
        let Ok(signature) = base64::engine::general_purpose::STANDARD.decode(signature) else {
            continue;
        };
        if matches!(
            creds::verify(handle, creds::Algorithm::HmacSha256, message, &signature),
            Ok(true)
        ) {
            return true;
        }
    }
    false
}

fn header(headers: &Headers, name: &str) -> Option<String> {
    headers
        .get(name)
        .into_iter()
        .next()
        .and_then(|value| String::from_utf8(value).ok())
}

fn safe_headers(headers: &Headers) -> BTreeMap<String, String> {
    let mut safe = BTreeMap::new();
    for (name, value) in headers.entries() {
        if matches!(
            name.as_str(),
            "authorization"
                | "cookie"
                | "webhook-signature"
                | "x-api-key"
                | "x-confluence-token"
                | "x-fpl-ingress-secret"
        ) {
            continue;
        }
        if (matches!(name.as_str(), "content-type" | "user-agent") || name.starts_with("x-"))
            && let Ok(value) = String::from_utf8(value)
        {
            safe.insert(name, value);
        }
    }
    safe
}

fn json_error(status: u16) -> String {
    serde_json::json!({
        "status": status,
        "request_id": serde_json::Value::Null,
        "duplicate": false,
    })
    .to_string()
}

fn reply(response_out: ResponseOutparam, status: u16, body: String, allow: &str) -> u16 {
    let headers = Headers::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    if !allow.is_empty() {
        let _ = headers.set("allow", &[allow.as_bytes().to_vec()]);
    }
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let Ok(response_body) = response.body() else {
        return 500;
    };
    ResponseOutparam::set(response_out, Ok(response));

    let mut out = response_body.write().expect("response stream");
    out.write_all(body.as_bytes()).expect("response body");
    out.flush().expect("flush response body");
    drop(out);
    OutgoingBody::finish(response_body, None).expect("finish response body");
    status
}

wasip2::http::proxy::export!(Confluence);
