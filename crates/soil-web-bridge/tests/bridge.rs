//! End to end: a real socket, a real records store, a real WebSocket client.
//!
//! The point of these tests is not coverage of `soil:records` (that crate has
//! its own) but the two things the bridge adds: the grant check happens before
//! the store is touched, and the duplex session carries both a call's result and
//! the change another session caused.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use soil_web_bridge::{Auth, Bridge, GrantSet, RecordsCapability, Registry, Server};
use tempfile::TempDir;
use tungstenite::Message;
use tungstenite::handshake::client::generate_key;
use tungstenite::http::Request;

const READS: (&str, &str) = ("soil:records.read", "read the fixture collection");
const WRITES: (&str, &str) = ("soil:records.write", "write the fixture collection");

fn fixture(dir: &Path, grants: &[(&str, &str)]) -> std::path::PathBuf {
    let spore = dir.join("spore");
    std::fs::create_dir_all(&spore).unwrap();
    let mut declared = String::new();
    for (key, reason) in grants {
        declared.push_str(&format!("\"{key}\" = \"{reason}\"\n"));
    }
    let manifest = format!(
        r#"schema = 1

[spore]
id = "dev.fungos.bridge-fixture"
name = "bridge-fixture"
version = "0.1.0"
license = "MIT"
summary = "A manifest that carries grants for the bridge tests"

[engine]
soil = ">=0.1"
target = "wasm32-wasip2"

[launch]
default = "canvas"
surfaces = ["canvas"]

[ui]
entry = "ui/view.lua"

[grants]
{declared}"#
    );
    std::fs::write(spore.join("spore.toml"), manifest).unwrap();
    spore
}

fn start(grants: &[(&str, &str)]) -> (TempDir, Server) {
    let (dir, _bridge, server) = start_with(grants, Auth::loopback_only());
    (dir, server)
}

fn start_with(grants: &[(&str, &str)], auth: Auth) -> (TempDir, Arc<Bridge>, Server) {
    let dir = tempfile::tempdir().unwrap();
    let spore = fixture(dir.path(), grants);
    let grants = GrantSet::load(&spore).unwrap();
    let records =
        RecordsCapability::open(&dir.path().join("records"), "worklog", "worklog.event").unwrap();
    let bridge = Arc::new(
        Bridge::new(
            "dev.fungos.bridge-fixture",
            grants,
            Registry::new().with(records),
        )
        .with_auth(auth),
    );
    let server = bridge
        .clone()
        .serve("127.0.0.1:0".parse().unwrap())
        .expect("bind an ephemeral port");
    (dir, bridge, server)
}

fn call(id: &str, interface: &str, function: &str, params: Value) -> Value {
    json!({
        "v": 1,
        "call": { "id": id, "interface": interface, "function": function, "params": params }
    })
}

fn entry(id: &str, body: &str) -> Value {
    json!({
        "entry": {
            "key": { "collection": "worklog.event", "id": id },
            "schema": "worklog/event@1",
            "body": BASE64.encode(body),
        }
    })
}

/// One request, one response. The server closes the connection, so reading to
/// EOF is the whole protocol.
fn post(addr: SocketAddr, body: &Value, token: Option<&str>) -> (u16, Value) {
    post_to(addr, "/v1/call", body, token)
}

fn post_to(addr: SocketAddr, path: &str, body: &Value, token: Option<&str>) -> (u16, Value) {
    let payload = serde_json::to_vec(body).unwrap();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        payload.len()
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("\r\n");

    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(&payload).unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").expect("a response head");
    let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
    // Transport failures answer with plain text (401, 411, 413, ...); capability
    // errors answer with a JSON envelope. The helper accepts both.
    let body = if body.trim_start().starts_with('{') {
        serde_json::from_str(body).unwrap()
    } else {
        Value::Null
    };
    (status, body)
}

fn session(addr: SocketAddr) -> tungstenite::WebSocket<TcpStream> {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let request = Request::builder()
        .uri(format!("ws://{addr}/v1/session"))
        .header("Host", addr.to_string())
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())
        .unwrap();
    let (socket, _) = tungstenite::client(request, stream).unwrap();
    socket
}

fn next_frame(socket: &mut tungstenite::WebSocket<TcpStream>) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "no frame arrived");
        match socket.read() {
            Ok(Message::Text(text)) => return serde_json::from_str(text.as_str()).unwrap(),
            Ok(_) => continue,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => panic!("session failed: {error}"),
        }
    }
}

#[test]
fn a_record_round_trips_over_http() {
    let (_dir, server) = start(&[READS, WRITES]);
    let addr = server.addr();

    let (status, body) = post(
        addr,
        &call(
            "1",
            "soil:records",
            "upsert",
            entry("a", r#"{"message":"hi"}"#),
        ),
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["id"], "1", "the correlation id is echoed");
    assert!(body["result"]["ok"]["revision"].as_u64().unwrap() >= 1);

    let (status, body) = post(
        addr,
        &call(
            "2",
            "soil:records",
            "get",
            json!({"key":{"collection":"worklog.event","id":"a"}}),
        ),
        None,
    );
    assert_eq!(status, 200, "{body}");
    let encoded = body["result"]["ok"]["body"].as_str().unwrap();
    assert_eq!(
        String::from_utf8(BASE64.decode(encoded).unwrap()).unwrap(),
        r#"{"message":"hi"}"#
    );

    let (status, body) = post(
        addr,
        &call(
            "3",
            "soil:records",
            "scan",
            json!({"collection":"worklog.event","limit":10}),
        ),
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["ok"]["entries"][0]["key"]["id"], "a");
}

#[test]
fn a_read_only_manifest_cannot_write_and_says_so() {
    let (_dir, server) = start(&[READS]);
    let addr = server.addr();

    let (status, body) = post(
        addr,
        &call(
            "1",
            "soil:records",
            "upsert",
            entry("a", r#"{"message":"hi"}"#),
        ),
        None,
    );
    assert_eq!(status, 200, "a refusal is a WIT error, not an HTTP one");
    assert_eq!(body["result"]["err"]["code"], "denied");
    assert!(
        body["result"]["err"]["message"]
            .as_str()
            .unwrap()
            .contains("soil:records.write")
    );

    let (status, body) = post(
        addr,
        &call(
            "2",
            "soil:records",
            "scan",
            json!({"collection":"worklog.event"}),
        ),
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["ok"]["entries"], json!([]));
}

#[test]
fn an_interface_this_host_does_not_serve_is_unsupported() {
    let (_dir, server) = start(&[READS, WRITES]);
    let (status, body) = post(
        server.addr(),
        &call("1", "soil:vfs", "read", json!({})),
        None,
    );
    assert_eq!(status, 200);
    assert_eq!(body["result"]["err"]["code"], "unsupported");
}

#[test]
fn a_bad_protocol_version_is_a_transport_error() {
    let (_dir, server) = start(&[READS]);
    let mut request = call(
        "1",
        "soil:records",
        "scan",
        json!({"collection":"worklog.event"}),
    );
    request["v"] = json!(9);
    let (status, body) = post(server.addr(), &request, None);
    assert_eq!(status, 400);
    assert_eq!(body["result"]["err"]["code"], "invalid");
}

#[test]
fn a_second_session_sees_a_change_the_first_one_made() {
    let (_dir, server) = start(&[("soil:records", "the whole package")]);
    let addr = server.addr();

    let mut writer = session(addr);
    let mut watcher = session(addr);

    let ready = next_frame(&mut writer);
    assert_eq!(ready["event"]["kind"], "ready");
    assert_eq!(ready["event"]["spore"], "dev.fungos.bridge-fixture");
    assert_eq!(
        ready["event"]["interfaces"]["soil:records"]["collection"],
        "worklog.event"
    );
    let ready = next_frame(&mut watcher);
    assert_eq!(ready["event"]["kind"], "ready");

    writer
        .send(Message::text(
            serde_json::to_string(&call(
                "9",
                "soil:records",
                "upsert",
                entry("b", r#"{"message":"live"}"#),
            ))
            .unwrap(),
        ))
        .unwrap();

    let response = next_frame(&mut writer);
    assert_eq!(response["id"], "9");
    assert!(response["result"]["ok"].is_object(), "{response}");

    let changed = next_frame(&mut watcher);
    assert_eq!(changed["event"]["kind"], "changed");
    assert_eq!(changed["event"]["id"], "b");
    assert!(changed["event"]["revision"].as_u64().unwrap() >= 1);
}

#[test]
fn a_conditional_write_loses_cleanly_and_the_change_feed_shows_it() {
    let (_dir, server) = start(&[("soil:records", "the whole package")]);
    let addr = server.addr();
    let body = BASE64.encode(r#"{"message":"cas"}"#);

    // Create-if-absent: `expected` is omitted.
    let (status, response) = post(
        addr,
        &call(
            "1",
            "soil:records",
            "compare-and-swap",
            json!({
                "key": { "collection": "worklog.event", "id": "cas" },
                "entry": {
                    "key": { "collection": "worklog.event", "id": "cas" },
                    "schema": "worklog/event@1",
                    "body": body,
                },
            }),
        ),
        None,
    );
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["result"]["ok"]["applied"], true, "{response}");

    // A stale expectation loses. The host returns the current entry instead of
    // overwriting the winner, which is the whole point of the conditional
    // write; sending the *current* version would have succeeded.
    let (status, response) = post(
        addr,
        &call(
            "2",
            "soil:records",
            "compare-and-swap",
            json!({
                "key": { "collection": "worklog.event", "id": "cas" },
                "expected": { "revision": 1, "origin": "stale" },
                "entry": {
                    "key": { "collection": "worklog.event", "id": "cas" },
                    "schema": "worklog/event@1",
                    "body": body,
                },
            }),
        ),
        None,
    );
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["result"]["ok"]["applied"], false, "{response}");
    assert_eq!(
        response["result"]["ok"]["entry"]["key"]["id"], "cas",
        "a conflict returns the authoritative entry"
    );

    let (status, response) = post(
        addr,
        &call("3", "soil:records", "changes", json!({ "limit": 10 })),
        None,
    );
    assert_eq!(status, 200, "{response}");
    let entries = response["result"]["ok"]["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| entry["key"]["id"] == "cas"),
        "{response}"
    );
}

#[test]
fn the_change_feed_needs_the_sync_grant() {
    let (_dir, server) = start(&[READS]);
    let (status, body) = post(
        server.addr(),
        &call("1", "soil:records", "changes", json!({ "limit": 10 })),
        None,
    );
    assert_eq!(status, 200);
    assert_eq!(body["result"]["err"]["code"], "denied");
    assert!(
        body["result"]["err"]["message"]
            .as_str()
            .unwrap()
            .contains("soil:records.sync")
    );
}

#[test]
fn a_session_can_provide_soil_http_and_answer_a_node_call() {
    let (_dir, server) = start(&[READS, ("soil:http", "outbound fetch")]);
    let addr = server.addr();

    let mut browser = session(addr);
    assert_eq!(next_frame(&mut browser)["event"]["kind"], "ready");

    browser
        .send(Message::text(
            json!({ "v": 1, "provide": { "id": "p1", "packages": ["soil:http"] } }).to_string(),
        ))
        .unwrap();
    let ack = next_frame(&mut browser);
    assert_eq!(ack["id"], "p1", "{ack}");
    assert_eq!(ack["result"]["ok"]["packages"][0], "soil:http");

    // The node calls into the page while the page answers on this thread.
    let caller = std::thread::spawn(move || {
        post_to(
            addr,
            "/v1/provider/call",
            &json!({
                "package": "soil:http",
                "function": "fetch",
                "params": { "url": "https://example.com" },
                "timeoutMs": 5000,
            }),
            None,
        )
    });

    let call = next_frame(&mut browser);
    assert_eq!(call["call"]["interface"], "soil:http");
    assert_eq!(call["call"]["function"], "fetch");
    let id = call["call"]["id"].as_str().unwrap();
    assert!(
        id.starts_with('~'),
        "host ids must not collide with page ids"
    );

    browser
        .send(Message::text(
            json!({
                "v": 1,
                "id": id,
                "result": { "ok": {
                    "status": 200,
                    "finalUrl": "https://example.com/",
                    "headers": [],
                    "body": BASE64.encode("hello from the browser"),
                } },
            })
            .to_string(),
        ))
        .unwrap();

    let (status, body) = caller.join().unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["ok"]["status"], 200, "{body}");
    let returned = body["result"]["ok"]["body"].as_str().unwrap();
    assert_eq!(
        String::from_utf8(BASE64.decode(returned).unwrap()).unwrap(),
        "hello from the browser"
    );
}

#[test]
fn a_provider_that_never_answers_times_out_and_is_told_to_cancel() {
    let (_dir, server) = start(&[("soil:http", "outbound fetch")]);
    let addr = server.addr();

    let mut browser = session(addr);
    next_frame(&mut browser);
    browser
        .send(Message::text(
            json!({ "v": 1, "provide": { "id": "p1", "packages": ["soil:http"] } }).to_string(),
        ))
        .unwrap();
    assert!(next_frame(&mut browser)["result"]["ok"].is_object());

    let caller = std::thread::spawn(move || {
        post_to(
            addr,
            "/v1/provider/call",
            &json!({
                "package": "soil:http",
                "function": "fetch",
                "params": { "url": "https://example.com" },
                "timeoutMs": 200,
            }),
            None,
        )
    });

    let call = next_frame(&mut browser);
    let called = call["call"]["id"].as_str().unwrap().to_owned();
    // Deliberately do not answer. The host gives up and says so.
    let cancel = next_frame(&mut browser);
    assert_eq!(cancel["cancel"]["id"], called, "{cancel}");

    let (status, body) = caller.join().unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["err"]["code"], "timeout", "{body}");
}

#[test]
fn a_session_cannot_provide_an_interface_the_spore_did_not_ask_for() {
    let (_dir, server) = start(&[READS]);
    let mut browser = session(server.addr());
    next_frame(&mut browser);

    browser
        .send(Message::text(
            json!({ "v": 1, "provide": { "id": "p1", "packages": ["soil:http"] } }).to_string(),
        ))
        .unwrap();
    let ack = next_frame(&mut browser);
    assert_eq!(ack["result"]["err"]["code"], "denied", "{ack}");
    assert!(
        ack["result"]["err"]["message"]
            .as_str()
            .unwrap()
            .contains("soil:http")
    );
}

#[test]
fn one_session_answers_per_package() {
    let (_dir, server) = start(&[("soil:http", "outbound fetch")]);
    let addr = server.addr();

    let mut first = session(addr);
    next_frame(&mut first);
    first
        .send(Message::text(
            json!({ "v": 1, "provide": { "id": "a", "packages": ["soil:http"] } }).to_string(),
        ))
        .unwrap();
    assert!(next_frame(&mut first)["result"]["ok"].is_object());

    let mut second = session(addr);
    next_frame(&mut second);
    second
        .send(Message::text(
            json!({ "v": 1, "provide": { "id": "b", "packages": ["soil:http"] } }).to_string(),
        ))
        .unwrap();
    let ack = next_frame(&mut second);
    assert_eq!(ack["result"]["err"]["code"], "denied", "{ack}");
    assert!(
        ack["result"]["err"]["message"]
            .as_str()
            .unwrap()
            .contains("another session")
    );
}

#[test]
fn a_provider_call_with_no_provider_is_unavailable() {
    let (_dir, server) = start(&[("soil:http", "outbound fetch")]);
    let (status, body) = post_to(
        server.addr(),
        "/v1/provider/call",
        &json!({ "package": "soil:http", "function": "fetch", "params": {} }),
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["err"]["code"], "unavailable", "{body}");
}

#[test]
fn a_spore_fetch_goes_through_the_session_when_one_is_offered() {
    use soil_http_host::{HttpTransport, TransportError, TransportRequest};
    use soil_web_bridge::SessionTransport;

    let (_dir, bridge, server) =
        start_with(&[("soil:http", "outbound fetch")], Auth::loopback_only());
    let addr = server.addr();

    let mut browser = session(addr);
    next_frame(&mut browser);
    browser
        .send(Message::text(
            json!({ "v": 1, "provide": { "id": "p1", "packages": ["soil:http"] } }).to_string(),
        ))
        .unwrap();
    assert!(next_frame(&mut browser)["result"]["ok"].is_object());

    let request = TransportRequest::from_parts(
        "https://example.com/data",
        "get",
        vec![],
        None,
        Duration::from_secs(2),
        1024,
    )
    .unwrap();
    let transport = SessionTransport::new(bridge.clone());
    let caller = std::thread::spawn(move || transport.fetch(&request));

    // The page answers the fetch, so the Spore's transport call returns the
    // page's response rather than the node's.
    let call = next_frame(&mut browser);
    assert_eq!(call["call"]["interface"], "soil:http");
    assert_eq!(call["call"]["params"]["method"], "get");
    assert_eq!(call["call"]["params"]["url"], "https://example.com/data");
    let id = call["call"]["id"].as_str().unwrap().to_owned();
    browser
        .send(Message::text(
            json!({
                "v": 1,
                "id": id,
                "result": { "ok": {
                    "status": 201,
                    "finalUrl": "https://example.com/data",
                    "headers": [{ "name": "content-type", "value": "application/json" }],
                    "body": BASE64.encode("from the page"),
                } },
            })
            .to_string(),
        ))
        .unwrap();

    let response = caller.join().unwrap().expect("the page answered");
    assert_eq!(response.status, 201);
    assert_eq!(response.body, b"from the page");
    assert_eq!(response.headers[0].0, "content-type");

    // With the page gone the transport says "not me" — which is what lets a
    // composed transport fall back to the node's own network.
    drop(browser);
    let deadline = Instant::now() + Duration::from_secs(5);
    while bridge.provides("soil:http") {
        assert!(Instant::now() < deadline, "the provider was never released");
        std::thread::sleep(Duration::from_millis(20));
    }
    let absent = TransportRequest::from_parts(
        "https://example.com/data",
        "get",
        vec![],
        None,
        Duration::from_secs(2),
        1024,
    )
    .unwrap();
    assert_eq!(
        SessionTransport::new(bridge.clone())
            .fetch(&absent)
            .unwrap_err(),
        TransportError::Unsupported
    );
}

#[test]
fn a_configured_token_is_required_on_both_planes() {
    let (_dir, _bridge, server) = start_with(&[READS], Auth::with_token("s3cret"));
    let addr = server.addr();
    let scan = call(
        "1",
        "soil:records",
        "scan",
        json!({"collection":"worklog.event"}),
    );

    let (status, _) = post(addr, &scan, None);
    assert_eq!(status, 401);
    let (status, body) = post(addr, &scan, Some("s3cret"));
    assert_eq!(status, 200, "{body}");

    // A browser cannot set an Authorization header on a WebSocket, so the query
    // parameter is the one way a page can present a token.
    let unauthenticated = TcpStream::connect(addr).unwrap();
    let request = Request::builder()
        .uri(format!("ws://{addr}/v1/session"))
        .header("Host", addr.to_string())
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())
        .unwrap();
    assert!(tungstenite::client(request, unauthenticated).is_err());
}
