//! Minimal MCP (streamable HTTP) server for Operator.
//!
//! `mush_exec` runs a Mush command line against the daemon's fixed VFS root,
//! with the same surface the local shell has: VFS builtins and the allowlisted
//! host programs (git, cargo, rg, sed, ...) that run against an overlay.
//! Authentication is a bearer token because the endpoint is on loopback but
//! still grants shell access to the workspace — including those host programs,
//! which run with the daemon's privileges.

use std::io::Read;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tiny_http::{Header, Response, Server, StatusCode};

use crate::job::{Jobs, Outcome};

const MAX_REQUEST: u64 = 2 * 1024 * 1024;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;

pub fn serve(server: Server, token: &str, jobs: std::sync::Arc<Jobs>) -> Result<()> {
    eprintln!("mush: mcp server http://{}/mcp", server.server_addr());
    for request in server.incoming_requests() {
        // A request that runs a command can take minutes; handling requests
        // one at a time would make `mush_job` and `mush_cancel` unreachable
        // exactly when they are needed.
        let token = token.to_string();
        let jobs = std::sync::Arc::clone(&jobs);
        std::thread::spawn(move || {
            if let Err(error) = handle(request, &token, &jobs) {
                eprintln!("mush: mcp: {error:#}");
            }
        });
    }
    Ok(())
}

fn handle(mut request: tiny_http::Request, token: &str, jobs: &Jobs) -> Result<()> {
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
                        "description": "Execute a Mush command line in the daemon's fixed VFS workspace: VFS builtins, records, installed wasm commands, and the allowlisted host programs (git, cargo, rg, sed, make) which run against an overlay of the VFS. A command that outlives the daemon deadline is interrupted and returns state=timeout; `mush_cancel` stops one early. A command that outlives its background threshold keeps running and returns state=background with a job id.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "command": {"type": "string", "description": "Shell command line(s)"}
                            },
                            "required": ["command"]
                        }
                    }, {
                        "name": "mush_job",
                        "description": "Check a backgrounded Mush command, or list every job when no id is given. Use this after mush_exec returns state=background instead of re-running the command.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "job": {"type": "string", "description": "Job id from mush_exec; omit to list"}
                            }
                        }
                    }, {
                        "name": "mush_cancel",
                        "description": "Stop a Mush job: queued work is dropped, running work is interrupted (guest or host program). Use after mush_exec returns state=background, or to stop a command that is taking too long.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "job": {"type": "string", "description": "Job id from mush_exec or mush_job"}
                            },
                            "required": ["job"]
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
            if name == "mush_job" {
                let job = params
                    .get("arguments")
                    .and_then(|value| value.get("job"))
                    .and_then(Value::as_str);
                let body = match job {
                    Some(id) => jobs
                        .snapshot(id)
                        .unwrap_or_else(|| json!({"error": format!("no job {id}")})),
                    None => jobs.list(),
                };
                let text = serde_json::to_string_pretty(&body)?;
                return respond_json(
                    request,
                    200,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": text}],
                            "structuredContent": body,
                            "isError": false
                        }
                    }),
                );
            }
            if name == "mush_cancel" {
                let job = params
                    .get("arguments")
                    .and_then(|value| value.get("job"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let (body, is_error) = match jobs.cancel(job) {
                    Ok(response) => (response, false),
                    Err(error) => (json!({"ok": false, "error": format!("{error:#}")}), true),
                };
                let text = serde_json::to_string_pretty(&body)?;
                return respond_json(
                    request,
                    200,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": text}],
                            "structuredContent": body,
                            "isError": is_error
                        }
                    }),
                );
            }
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
            let outcome = jobs.submit(command, None, true, None)?;
            let (text, structured, is_error) = match outcome {
                Outcome::Completed(executed) => {
                    let mut stdout = executed.output;
                    if stdout.len() > MAX_OUTPUT {
                        stdout.truncate(MAX_OUTPUT);
                        stdout.push_str("\n[mush: output truncated]\n");
                    }
                    let text = format!("{}{}", stdout, executed.stderr);
                    let failed = executed.status != 0 || executed.exit;
                    (
                        text,
                        json!({
                            "stdout": stdout,
                            "stderr": executed.stderr,
                            "status": executed.status,
                            "exit": executed.exit,
                            "state": executed.state.as_str(),
                            "blocking": executed.state.blocking()
                        }),
                        failed,
                    )
                }
                Outcome::Background { job, after } => {
                    let hint = format!(
                        "command still running after {}s; check it with mush_job {{\"job\":\"{job}\"}} instead of re-running it",
                        after.as_secs()
                    );
                    (
                        hint.clone(),
                        json!({
                            "state": "background",
                            "job": job,
                            "after_secs": after.as_secs(),
                            "blocking": false,
                            "hint": hint,
                        }),
                        false,
                    )
                }
            };
            respond_json(
                request,
                200,
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": text}],
                        "structuredContent": structured,
                        "isError": is_error
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
