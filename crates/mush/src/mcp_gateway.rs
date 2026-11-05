//! Loopback MCP/DCP tool gateway.
//!
//! Operator talks MCP to this process. It restores scrubbed values only for
//! explicitly allowed tool names, forwards to the host-selected unibus-mcp
//! bridge, and scrubs the result before returning it to the model.

use std::{io::Read, thread};

use anyhow::{Context, Result};
use soil_scrub::Scrubber;
use tiny_http::{Header, Response, Server, StatusCode};

use crate::gateway::SharedScrubber;

pub struct McpGateway {
    pub url: String,
    pub token: String,
}

pub fn start(
    upstream: String,
    upstream_token: Option<String>,
    token: Option<String>,
    scrubber: Option<SharedScrubber>,
    allowed_tools: Vec<String>,
) -> Result<McpGateway> {
    let server = Server::http("127.0.0.1:0").map_err(|error| anyhow::anyhow!("{error}"))?;
    let address = server
        .server_addr()
        .to_ip()
        .context("MCP gateway did not bind an IP socket")?;
    let session_token = token.unwrap_or_else(new_token);
    let expected = session_token.clone();
    thread::Builder::new()
        .name("soil-mcp-gateway".into())
        .spawn(move || {
            for request in server.incoming_requests() {
                if let Err(error) = handle(
                    request,
                    &upstream,
                    upstream_token.as_deref(),
                    &expected,
                    scrubber.as_ref(),
                    &allowed_tools,
                ) {
                    eprintln!("soil mcp: {error:#}");
                }
            }
        })
        .context("start MCP gateway")?;
    Ok(McpGateway {
        url: format!("http://{address}/mcp"),
        token: session_token,
    })
}

fn handle(
    mut request: tiny_http::Request,
    upstream: &str,
    upstream_token: Option<&str>,
    token: &str,
    scrubber: Option<&SharedScrubber>,
    allowed_tools: &[String],
) -> Result<()> {
    if request.method().as_str() != "POST" || request.url() != "/mcp" {
        return respond(request, 404, &[], b"not found\n");
    }
    if !authorized(&request, token) {
        return respond(request, 401, &[], b"unauthorized\n");
    }
    let mut body = Vec::new();
    request
        .as_reader()
        .take(8 * 1024 * 1024)
        .read_to_end(&mut body)?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&body).context("MCP request is not JSON")?;
    let is_call = value.get("method").and_then(serde_json::Value::as_str) == Some("tools/call");
    if is_call && let Some(params) = value.get_mut("params") {
        let allowed = params
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| {
                allowed_tools
                    .iter()
                    .any(|allowed| allowed == name || allowed == "*")
            });
        if allowed
            && let Some(args) = params.get_mut("arguments")
            && let Some(scrubber) = scrubber
        {
            let scrubber = scrubber.lock().expect("scrubber poisoned");
            restore_json(args, &scrubber);
        }
    }
    let body = serde_json::to_vec(&value)?;

    let mut outgoing = ureq::post(upstream)
        .set("content-type", "application/json")
        .set("accept", "application/json");
    if let Some(key) = upstream_token {
        outgoing = outgoing.set("authorization", &format!("Bearer {key}"));
    }
    if let Some(session) = header(&request, "mcp-session-id") {
        outgoing = outgoing.set("mcp-session-id", &session);
    }
    let response = match outgoing.send_bytes(&body) {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let mut bytes = Vec::new();
            let _ = response.into_reader().read_to_end(&mut bytes);
            return respond(request, status, &[], &bytes);
        }
        Err(error) => {
            return respond(
                request,
                502,
                &[],
                format!("unibus-mcp error: {error}\n").as_bytes(),
            );
        }
    };
    let status = response.status();
    let content_type = response
        .header("content-type")
        .unwrap_or("application/json")
        .to_string();
    let session = response.header("mcp-session-id").map(str::to_string);
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(8 * 1024 * 1024)
        .read_to_end(&mut bytes)?;
    if let Some(scrubber) = scrubber
        && let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        let mut scrubber = scrubber.lock().expect("scrubber poisoned");
        scrub_json(&mut value, &mut scrubber);
        bytes = serde_json::to_vec(&value)?;
    }
    let mut headers = Vec::new();
    if let Ok(header) = Header::from_bytes("content-type", content_type.as_bytes()) {
        headers.push(header);
    }
    if let Some(session) = session
        && let Ok(header) = Header::from_bytes("mcp-session-id", session.as_bytes())
    {
        headers.push(header);
    }
    respond(request, status, &headers, &bytes)
}

fn restore_json(value: &mut serde_json::Value, scrubber: &Scrubber) {
    match value {
        serde_json::Value::String(text) => *text = scrubber.restore(text, "display"),
        serde_json::Value::Array(values) => {
            for value in values {
                restore_json(value, scrubber);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                restore_json(value, scrubber);
            }
        }
        _ => {}
    }
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

fn respond(
    request: tiny_http::Request,
    status: u16,
    headers: &[Header],
    body: &[u8],
) -> Result<()> {
    let mut response = Response::from_data(body.to_vec()).with_status_code(StatusCode(status));
    for header in headers {
        response.add_header(header.clone());
    }
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

fn new_token() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

use rand::Rng;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_tool_restores_arguments_and_response_is_scrubbed() {
        use soil_scrub::{Kind, Scrubber};
        use std::sync::{Arc, Mutex};

        let upstream = Server::http("127.0.0.1:0").unwrap();
        let address = upstream.server_addr().to_ip().unwrap();
        let scrubber = Arc::new(Mutex::new(Scrubber::new()));
        let token = scrubber
            .lock()
            .unwrap()
            .tokenize(Kind::Email, "ada@example.com", &["display"]);
        let expected_token = token.clone();
        let server = thread::spawn(move || {
            let mut request = upstream.recv().unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert!(body.contains("ada@example.com"));
            request
                .respond(
                    Response::from_string(
                        r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"ada@example.com"}]}}"#,
                    )
                    .with_header(Header::from_bytes("content-type", "application/json").unwrap()),
                )
                .unwrap();
        });
        let gateway = start(
            format!("http://{address}/mcp"),
            None,
            Some("mcp-session".into()),
            Some(scrubber),
            vec!["records_upsert".into()],
        )
        .unwrap();
        let response = ureq::post(&gateway.url)
            .set("authorization", &format!("Bearer {}", gateway.token))
            .send_string(&format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"records_upsert","arguments":{{"email":"{token}"}}}}}}"#
            ))
            .unwrap();
        let mut body = String::new();
        response.into_reader().read_to_string(&mut body).unwrap();
        assert!(body.contains(&expected_token), "{body}");
        assert!(!body.contains("ada@example.com"), "{body}");
        server.join().unwrap();
    }

    #[test]
    fn restores_only_inside_an_allowed_tool() {
        let mut scrubber = Scrubber::new();
        let token = scrubber.tokenize(soil_scrub::Kind::Email, "ada@example.com", &["display"]);
        let mut args = serde_json::json!({"email": token});
        restore_json(&mut args, &scrubber);
        assert_eq!(args["email"], "ada@example.com");
    }
}
