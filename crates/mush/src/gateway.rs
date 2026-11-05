//! Loopback OpenAI-compatible inference gateway.
//!
//! Operator points at this process. The gateway scrubs request JSON, forwards to
//! the Soil-resolved upstream, and returns the model response. The upstream
//! credential never enters Operator's environment.

use std::{io::Read, thread};

use anyhow::{Context, Result};
use rand::Rng;
use soil_scrub::Scrubber;
use tiny_http::{Header, Response, Server, StatusCode};

pub struct Gateway {
    pub base_url: String,
    pub token: String,
}

pub fn start(upstream: String, upstream_key: Option<String>, scrub: bool) -> Result<Gateway> {
    let server = Server::http("127.0.0.1:0").map_err(|error| anyhow::anyhow!("{error}"))?;
    let address = server
        .server_addr()
        .to_ip()
        .context("gateway did not bind an IP socket")?;
    let token = new_token();
    let session_token = token.clone();
    thread::Builder::new()
        .name("soil-inference-gateway".into())
        .spawn(move || {
            let mut scrubber = scrub.then(Scrubber::new);
            for request in server.incoming_requests() {
                if let Err(error) = handle(
                    request,
                    &upstream,
                    upstream_key.as_deref(),
                    &session_token,
                    scrubber.as_mut(),
                ) {
                    eprintln!("soil inference: {error:#}");
                }
            }
        })
        .context("start inference gateway")?;
    Ok(Gateway {
        base_url: format!("http://{address}/v1"),
        token,
    })
}

fn handle(
    mut request: tiny_http::Request,
    upstream: &str,
    upstream_key: Option<&str>,
    token: &str,
    mut scrubber: Option<&mut Scrubber>,
) -> Result<()> {
    let path = request.url().to_string();
    if !path.starts_with("/v1/") && path != "/v1" {
        return respond(request, 404, "text/plain", b"not found\n");
    }
    if !authorized(&request, token) {
        return respond(request, 401, "text/plain", b"unauthorized\n");
    }
    let mut body = Vec::new();
    request
        .as_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut body)?;
    let body = if let Some(scrubber) = scrubber.as_deref_mut() {
        scrub_request(&body, scrubber)
    } else {
        body
    };
    let method = request.method().as_str().to_string();
    let content_type = header(&request, "content-type");
    let accept = header(&request, "accept");
    let url = join_url(upstream, &path);
    let agent = ureq::AgentBuilder::new().build();
    let mut outgoing = agent.request(&method, &url);
    if let Some(content_type) = content_type {
        outgoing = outgoing.set("content-type", &content_type);
    }
    if let Some(accept) = accept {
        outgoing = outgoing.set("accept", &accept);
    }
    if let Some(key) = upstream_key {
        outgoing = outgoing.set("authorization", &format!("Bearer {key}"));
    }
    let result = if method == "GET" || method == "HEAD" {
        outgoing.call()
    } else {
        outgoing.send_bytes(&body)
    };
    let response = match result {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let mut bytes = Vec::new();
            let _ = response.into_reader().read_to_end(&mut bytes);
            return respond(request, code, "application/json", &bytes);
        }
        Err(error) => {
            return respond(
                request,
                502,
                "text/plain",
                format!("upstream error: {error}\n").as_bytes(),
            );
        }
    };
    let status = response.status();
    let content_type = response
        .header("content-type")
        .unwrap_or("application/json")
        .to_string();
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut bytes)?;
    if let Some(scrubber) = scrubber {
        bytes = scrub_request(&bytes, scrubber);
    }
    respond(request, status, &content_type, &bytes)
}

fn respond(
    request: tiny_http::Request,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let header = Header::from_bytes("content-type", content_type)
        .map_err(|_| anyhow::anyhow!("invalid content type"))?;
    let response = Response::from_data(body.to_vec())
        .with_status_code(StatusCode(status))
        .with_header(header);
    request.respond(response)?;
    Ok(())
}

fn authorized(request: &tiny_http::Request, token: &str) -> bool {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("authorization"))
        .and_then(|header| header.value.as_str().strip_prefix("Bearer "))
        .is_some_and(|value| value == token)
}

fn header(request: &tiny_http::Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str().to_string())
}

fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    if base.ends_with("/v1") {
        let tail = path.strip_prefix("v1/").unwrap_or(path);
        format!("{base}/{tail}")
    } else {
        format!("{base}/{path}")
    }
}

fn scrub_request(body: &[u8], scrubber: &mut Scrubber) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(body) else {
        return body.to_vec();
    };
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(text) {
        scrub_json(&mut value, scrubber);
        return serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec());
    }
    scrubber.scrub(text).into_bytes()
}

fn scrub_json(value: &mut serde_json::Value, scrubber: &mut Scrubber) {
    match value {
        serde_json::Value::String(text) => *text = scrubber.scrub(text),
        serde_json::Value::Array(values) => {
            for value in values {
                scrub_json(value, scrubber);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                scrub_json(value, scrubber);
            }
        }
        _ => {}
    }
}

fn new_token() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_requests_and_injects_the_upstream_key() {
        let upstream = Server::http("127.0.0.1:0").unwrap();
        let address = upstream.server_addr().to_ip().unwrap();
        let server =
            thread::spawn(move || {
                let mut request = upstream.recv().unwrap();
                let auth = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("authorization"))
                    .map(|header| header.value.as_str().to_string());
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                assert_eq!(auth.as_deref(), Some("Bearer upstream-secret"));
                assert!(!body.contains("ada@example.com"));
                assert!(body.contains("⟦mush:email:"));
                request
                    .respond(Response::from_string(r#"{"ok":true}"#).with_header(
                        Header::from_bytes("content-type", "application/json").unwrap(),
                    ))
                    .unwrap();
            });

        let gateway = start(
            format!("http://{address}"),
            Some("upstream-secret".into()),
            true,
        )
        .unwrap();
        let response = ureq::post(&format!("{}/chat/completions", gateway.base_url))
            .set("authorization", &format!("Bearer {}", gateway.token))
            .send_string(r#"{"messages":[{"role":"user","content":"mail ada@example.com"}]}"#)
            .unwrap();
        assert_eq!(response.status(), 200);
        server.join().unwrap();
    }

    #[test]
    fn joins_upstream_v1_paths_without_duplication() {
        assert_eq!(
            join_url("http://127.0.0.1:8096/v1", "/v1/models"),
            "http://127.0.0.1:8096/v1/models"
        );
        assert_eq!(
            join_url("http://127.0.0.1:8096", "/v1/chat/completions"),
            "http://127.0.0.1:8096/v1/chat/completions"
        );
    }
}
