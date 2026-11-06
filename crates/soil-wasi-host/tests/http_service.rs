use std::sync::Arc;

use soil_wasi_host::{HttpService, WasiHostBuilder, WasiHostPolicy};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

const ECHO: &[u8] = include_bytes!("../fixtures/echo.wasm");

#[tokio::test]
async fn serves_a_wasi_http_incoming_handler() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = Arc::new(
        HttpService::from_bytes(ECHO, WasiHostBuilder::new(WasiHostPolicy::default())).unwrap(),
    );
    let task = tokio::spawn(service.serve(listener));

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /hello HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("hello from wasi:http"), "{response}");

    task.abort();
}
