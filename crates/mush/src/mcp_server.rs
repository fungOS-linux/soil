//! Minimal MCP (streamable HTTP) server for Operator.
//!
//! One tool, `mush_exec`, executes a Mush command against the daemon's fixed
//! VFS root. Authentication is a bearer token because the endpoint is on
//! loopback but still grants shell access to the workspace.

use std::io::Read;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tiny_http::{Header, Response, Server, StatusCode};

use crate::outcome::{Executed, State};

const MAX_REQUEST: u64 = 2 * 1024 * 1024;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;

pub fn serve<F>(server: Server, token: &str, mut execute: F) -> Result<()>
where
    F: FnMut(&str) -> Result<Executed>,
{
    eprintln!("mush: mcp server http://{}/mcp", server.server_addr());
    for request in server.incoming_requests() {
        if let Err(error) = handle(request, token, &mut execute) {
            eprintln!("mush: mcp: {error:#}");
        }
    }
    Ok(())
}

fn handle<F>(mut request: tiny_http::Request, token: &str, execute: &mut F) -> Result<()>
where
    F: FnMut(&str) -> Result<Executed>,
{
    if request.method().as_str() != "POST" || request.url() != "/mcp" {
        return respond(request, 404, &[], b"not found\n");
    }
    if !authorized(&request, token) {
        return respond(request, 401, &[], b"unauthorized\n");
    }
    let mut body = Vec::new();
    request
        .as_reader()
        .take(MAX_REQUEST)
        .read_to_end(&mut body)?;
    let request_json: Value = serde_json::from_slice(&body).context("parse MCP JSON")?;
    let id = request_json.get("id").cloned().unwrap_or(Value::Null);
    let method = request_json
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "initialize" => respond_json(
            request,
            200,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "mush", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
        ),
        "notifications/initialized" => respond(request, 204, &[], b""),
        "tools/list" => respond_json(
            request,
            200,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [{
                        "name": "mush_exec",
                        "description": "Execute a Mush command in the daemon's fixed VFS workspace. Use for VFS, records and installed wasm commands; no host process is involved. A command that outlives the daemon deadline is interrupted and returns state=timeout rather than hanging.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "command": {"type": "string", "description": "Shell command line(s)"}
                            },
                            "required": ["command"]
                        }
                    }]
                }
            }),
        ),
        "tools/call" => {
            let params = request_json.get("params").cloned().unwrap_or(Value::Null);
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name != "mush_exec" {
                return respond_json(
                    request,
                    200,
                    &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("unknown tool: {name}")}}),
                );
            }
            let command = params
                .get("arguments")
                .and_then(|value| value.get("command"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if command.is_empty() {
                return respond_json(
                    request,
                    200,
                    &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"command is required"}}),
                );
            }
            let (mut stdout, mut stderr, mut exit, mut status) =
                (String::new(), String::new(), false, 0);
            let mut state = State::Done;
            for line in command.lines() {
                let executed = execute(line)?;
                stdout.push_str(&executed.output);
                stderr.push_str(&executed.stderr);
                exit |= executed.exit;
                status = executed.status;
                if executed.state != State::Done {
                    state = executed.state;
                }
                if stdout.len() > MAX_OUTPUT {
                    stdout.truncate(MAX_OUTPUT);
                    stdout.push_str("\n[mush: output truncated]\n");
                    status = 1;
                    break;
                }
                if exit {
                    break;
                }
            }
            let text = format!("{stdout}{stderr}");
            respond_json(
                request,
                200,
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": text}],
                        "structuredContent": {
                            "stdout": stdout,
                            "stderr": stderr,
                            "status": status,
                            "exit": exit,
                            "state": state.as_str(),
                            "blocking": state.blocking()
                        },
                        "isError": status != 0 || exit || state != State::Done
                    }
                }),
            )
        }
        _ => respond_json(
            request,
            200,
            &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("unknown method: {method}")}}),
        ),
    }
}

fn respond_json(request: tiny_http::Request, status: u16, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let headers = [Header::from_bytes("content-type", "application/json").unwrap()];
    respond(request, status, &headers, &body)
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
