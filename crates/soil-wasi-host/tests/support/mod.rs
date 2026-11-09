//! Shared HTTP helpers for the host integration tests.

#![allow(dead_code)]

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

/// Send one raw HTTP/1.1 request and return the full response bytes.
pub async fn send(address: std::net::SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

/// Read a response body, decoding chunked framing when the guest leaves the
/// response length open.
pub fn response_body(response: &str) -> String {
    let (head, mut rest) = response.split_once("\r\n\r\n").unwrap();
    if !head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return rest.to_string();
    }
    let mut body = String::new();
    loop {
        let (size, after) = rest.split_once("\r\n").unwrap();
        let size = usize::from_str_radix(size, 16).unwrap();
        if size == 0 {
            return body;
        }
        body.push_str(&after[..size]);
        rest = &after[size + 2..];
    }
}

/// Build a POST request with explicit headers.
pub fn post(host: &str, path: &str, body: &str, headers: &[(&str, &str)]) -> String {
    let mut request = format!("POST {path} HTTP/1.1\r\nHost: {host}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    request
}
