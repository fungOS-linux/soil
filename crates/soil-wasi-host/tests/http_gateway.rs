use std::sync::Arc;

use http::Method;
use soil_wasi_host::{
    GatewayRoute, HttpGatewayBuilder, HttpService, WasiHostBuilder, WasiHostPolicy,
};
use tokio::net::TcpListener;

mod support;
use support::{post, response_body, send};

const ECHO: &[u8] = include_bytes!("../fixtures/echo.wasm");
const CONTEXT: &[u8] = include_bytes!("../fixtures/context.wasm");

fn service(name: &str, component: &[u8]) -> Arc<HttpService> {
    Arc::new(
        HttpService::from_bytes_named(
            name,
            component,
            WasiHostBuilder::new(WasiHostPolicy::default()),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn routes_by_vhost_path_and_method() {
    let gateway = HttpGatewayBuilder::new()
        .max_body_bytes(64)
        .vhost(
            "hooks.test",
            "echo",
            service("echo", ECHO),
            [GatewayRoute::new([Method::POST], "/webhooks/").unwrap()],
        )
        .unwrap()
        .build()
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(Arc::new(gateway).serve(listener));

    let response = send(
        address,
        &post(
            "hooks.test",
            "/webhooks/example",
            "{}",
            &[("content-type", "application/json")],
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("hello from wasi:http"), "{response}");

    let response = send(
        address,
        "GET /webhooks/example HTTP/1.1\r\nHost: hooks.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    assert!(
        response.to_ascii_lowercase().contains("allow: post"),
        "{response}"
    );

    let response = send(address, &post("hooks.test", "/admin", "{}", &[])).await;
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");

    let response = send(address, &post("other.test", "/webhooks/example", "{}", &[])).await;
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    assert!(!response.contains("hooks.test"), "{response}");

    let oversized = "x".repeat(128);
    let response = send(
        address,
        &post("hooks.test", "/webhooks/example", &oversized, &[]),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");

    task.abort();
}

#[tokio::test]
async fn context_is_host_attested() {
    let gateway = HttpGatewayBuilder::new()
        .vhost(
            "confluence.test",
            "context-fixture",
            service("context-fixture", CONTEXT),
            [GatewayRoute::new([Method::POST], "/webhooks/").unwrap()],
        )
        .unwrap()
        .build()
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(Arc::new(gateway).serve(listener));

    let response = send(
        address,
        &post(
            "confluence.test",
            "/webhooks/provider-a",
            "{}",
            &[("content-type", "application/json")],
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let body = response_body(&response);
    let fields: Vec<&str> = body.split('|').collect();
    assert_eq!(fields.len(), 6, "{body:?}");
    assert_eq!(fields[0].len(), 48, "request id: {}", fields[0]);
    assert_eq!(fields[1], "context-fixture");
    assert_eq!(fields[2], "/webhooks/");
    assert!(fields[3].starts_with("127.0.0.1:"), "peer: {}", fields[3]);
    assert_eq!(fields[4], "");
    let received: u64 = fields[5].parse().unwrap();
    assert!(received > 1_700_000_000_000, "received: {received}");

    task.abort();
}

#[test]
fn rejects_ambiguous_grants() {
    let echo = service("echo", ECHO);
    let route = || GatewayRoute::new([Method::POST], "/webhooks/").unwrap();

    let duplicate = HttpGatewayBuilder::new()
        .vhost("hooks.test", "echo", Arc::clone(&echo), [route()])
        .unwrap()
        .vhost("hooks.test", "echo-2", Arc::clone(&echo), [route()]);
    assert!(duplicate.is_err());

    assert!(
        HttpGatewayBuilder::new()
            .vhost("hooks.test", "echo", Arc::clone(&echo), [])
            .is_err()
    );
    assert!(GatewayRoute::new([Method::POST], "webhooks").is_err());
    assert!(GatewayRoute::new([], "/webhooks/").is_err());
    assert!(
        HttpGatewayBuilder::new()
            .vhost("hooks.test", "", Arc::clone(&echo), [route()])
            .is_err()
    );
    assert!(HttpGatewayBuilder::new().build().is_err());
}
