//! The bridge's HTTP surface, described from WIT.
//!
//! The document describes what the server actually answers — `POST /v1/call`
//! with one envelope, a WebSocket upgrade, a health check — and the *bodies* are
//! generated from the WIT. It is not a REST translation of the capability:
//! there is no `/records/scan`, because inventing one would be a second API for
//! the same call and the first thing to drift.
//!
//! One honest limitation: OpenAPI cannot correlate a response to a request, so
//! the `200` body is the union of every operation's result. The generated
//! TypeScript client can correlate them, which is why it is generated from the
//! same IR rather than from this document.

use serde_json::{Map, Value, json};

use crate::schema;
use crate::{Api, ApiFunction, pascal};

pub fn document(api: &Api) -> Value {
    let mut schemas = Map::new();
    for ty in &api.types {
        schemas.insert(ty.ts.clone(), annotated(schema::json(&ty.shape), &ty.docs));
    }
    schemas.insert("BridgeError".into(), bridge_error(api));
    schemas.insert("BridgeEvent".into(), bridge_event());

    let mut requests = Vec::new();
    let mut responses = Vec::new();
    for function in &api.functions {
        let (request, response) = names(function);
        schemas.insert(
            request.clone(),
            annotated(call_schema(api, function), &function.docs),
        );
        schemas.insert(response.clone(), result_schema(function));
        requests.push(json!({ "$ref": format!("#/components/schemas/{request}") }));
        responses.push(json!({ "$ref": format!("#/components/schemas/{response}") }));
    }

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": format!("{} over the Soil web bridge", api.package),
            "version": api.version,
            "description": api.docs.clone().unwrap_or_else(||
                format!("{} served by soil-web-bridge. Every call names a WIT interface and function; see the generated client for typed operations.", api.package)
            ),
        },
        "servers": [{ "url": "http://127.0.0.1:8795" }],
        // The bridge authenticates a *deployment*, never a person: a shared
        // bearer token for a tunnel, or nothing at all when only loopback can
        // reach it. The identity a Spore acts as is the gateway's business.
        "security": [{}, { "bearerAuth": [] }],
        "paths": {
            "/v1/call": {
                "post": {
                    "operationId": "call",
                    "summary": "One WIT call",
                    "description": "A capability failure arrives as 200 with result.err; HTTP errors are transport only.",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "oneOf": requests } } },
                    },
                    "responses": {
                        "200": {
                            "description": "The WIT result value, or a typed error",
                            "content": { "application/json": { "schema": { "oneOf": responses } } },
                        },
                        "400": { "description": "Malformed request or unsupported protocol version" },
                        "401": { "description": "No token, or a token that does not match" },
                        "403": { "description": "Origin is not in the allowlist" },
                        "411": { "description": "POST without Content-Length" },
                        "413": { "description": "Body over budget" },
                    },
                },
            },
            "/v1/session": {
                "get": {
                    "operationId": "session",
                    "summary": "WebSocket session",
                    "description": "Upgrade to a WebSocket. The host sends a ready event, answers the same call envelopes, and pushes changed events. Frames are text JSON with the same version.",
                    "responses": {
                        "101": { "description": "Switching Protocols" },
                        "401": { "description": "No token, or a token that does not match" },
                        "403": { "description": "Origin is not in the allowlist" },
                        "426": { "description": "Not a WebSocket upgrade" },
                        "503": { "description": "Too many sessions" },
                    },
                    "x-websocket-frames": {
                        "server": [
                            { "$ref": "#/components/schemas/BridgeEvent" },
                            { "oneOf": responses },
                        ],
                        "client": { "oneOf": requests },
                    },
                },
            },
            "/healthz": {
                "get": {
                    "operationId": "health",
                    "summary": "Liveness",
                    "responses": { "200": { "description": "ok" } },
                },
            },
        },
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "Shared token for this bridge deployment. A session may also pass it as `?token=` because a browser cannot set an Authorization header on a WebSocket. Absent means loopback-only access.",
                },
            },
            "schemas": schemas,
        },
    })
}

fn names(function: &ApiFunction) -> (String, String) {
    let suffix = pascal(&function.name);
    (format!("Call{suffix}"), format!("Result{suffix}"))
}
fn annotated(mut schema: Value, docs: &Option<String>) -> Value {
    if let Some(docs) = docs
        && let Some(object) = schema.as_object_mut()
        && !object.contains_key("$ref")
    {
        object.insert("description".into(), Value::String(docs.clone()));
    }
    schema
}

/// `{"v":1,"call":{"id":…,"interface":"soil:records","function":"scan","params":{…}}}`
fn call_schema(api: &Api, function: &ApiFunction) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for param in &function.params {
        let mut schema = schema::json(&param.shape);
        if let Some(docs) = &param.docs
            && let Some(object) = schema.as_object_mut()
        {
            object.insert("description".into(), Value::String(docs.clone()));
        }
        properties.insert(param.ts.clone(), schema);
        required.push(Value::String(param.ts.clone()));
    }

    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["v", "call"],
        "properties": {
            "v": { "const": 1 },
            "call": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "interface", "function", "params"],
                "properties": {
                    "id": { "type": "string" },
                    "interface": { "const": api.package },
                    "function": { "const": function.name },
                    "params": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": required,
                        "properties": properties,
                    },
                },
            },
        },
    })
}

/// `{"v":1,"id":"…","result":{"ok":…}}` or `{"result":{"err":…}}`.
fn result_schema(function: &ApiFunction) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["v", "id", "result"],
        "properties": {
            "v": { "const": 1 },
            "id": { "type": "string" },
            "result": {
                "oneOf": [
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["ok"],
                        "properties": { "ok": schema::json(&function.ok) },
                    },
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["err"],
                        "properties": { "err": { "$ref": "#/components/schemas/BridgeError" } },
                    },
                ],
            },
        },
    })
}

/// The error envelope, typed by the package's WIT `error` enum.
///
/// Every canonical package that reports failures declares `error` with the same
/// shape of cases, and the bridge's `CallError` carries one of them. A package
/// that grew a different name would fall back to `string` rather than invent a
/// union.
fn bridge_error(api: &Api) -> Value {
    let code = api
        .types
        .iter()
        .find(|ty| ty.name == "error")
        // By reference, not inlined: the component is the same contract the
        // generated client names, and a second copy would be free to drift.
        .map(|ty| json!({ "$ref": format!("#/components/schemas/{}", ty.ts) }))
        .unwrap_or_else(|| json!({ "type": "string" }));
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["code", "message"],
        "properties": { "code": code, "message": { "type": "string" } },
    })
}

/// The two frames a session may push. Protocol, not WIT: the host invents the
/// change feed, so the document and the generated client keep the same two
/// shapes by hand.
fn bridge_event() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "spore", "grants", "interfaces"],
                "properties": {
                    "kind": { "const": "ready" },
                    "spore": { "type": "string" },
                    "grants": { "type": "array", "items": { "type": "string" } },
                    "interfaces": { "type": "object", "additionalProperties": true },
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "collection", "id", "revision"],
                "properties": {
                    "kind": { "const": "changed" },
                    "collection": { "type": "string" },
                    "id": { "type": "string" },
                    "revision": { "type": "integer", "format": "uint64", "minimum": 0 },
                },
            },
        ],
    })
}

/// Every `$ref` target, for a test that proves they all resolve.
pub fn references(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if key == "$ref"
                    && let Some(reference) = value.as_str()
                {
                    out.push(reference.to_owned());
                }
                references(value, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                references(item, out);
            }
        }
        _ => {}
    }
}
