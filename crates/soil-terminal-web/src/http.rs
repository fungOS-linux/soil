//! Minimal HTTP/1.1 for the web terminal.
//!
//! The server is deliberately small: a request line, headers, and a bounded
//! body. It is not a general-purpose HTTP stack; it is the transport for one
//! page, one event stream, and two POST endpoints.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

use serde_json::Value;

/// Largest request body accepted. A pasted image is the large case; input and
/// resize are tiny.
pub const MAX_BODY: usize = 32 * 1024 * 1024;

pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The client declared more than [`MAX_BODY`]; the body was not read.
    pub too_large: bool,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub fn read_request(stream: &mut TcpStream) -> io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    let mut length = 0usize;
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let name = name.trim().to_owned();
            let value = value.trim().to_owned();
            if name.eq_ignore_ascii_case("content-length") {
                length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }
    let too_large = length > MAX_BODY;
    let mut body = vec![0u8; if too_large { 0 } else { length }];
    if !body.is_empty() {
        reader.read_exact(&mut body)?;
    }
    Ok(Some(Request {
        method,
        target,
        headers,
        body,
        too_large,
    }))
}

pub fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    stream.write_all(
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    stream.write_all(body)?;
    stream.flush()
}

pub fn respond_json(stream: &mut TcpStream, status: &str, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    respond(stream, status, "application/json; charset=utf-8", &body)
}

pub fn query_param(target: &str, name: &str) -> Option<String> {
    let (_, query) = target.split_once('?')?;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return Some(percent_decode(value));
        }
    }
    None
}

pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    out.push(byte as char);
                    index += 3;
                    continue;
                }
                out.push('%');
                index += 1;
            }
            b'+' => {
                out.push(' ');
                index += 1;
            }
            byte => {
                out.push(byte as char);
                index += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_query_value() {
        assert_eq!(
            query_param("/events?token=abc%20123&x=1", "token"),
            Some("abc 123".into())
        );
        assert_eq!(query_param("/events", "token"), None);
    }

    #[test]
    fn headers_are_case_insensitive() {
        let request = Request {
            method: "POST".into(),
            target: "/paste".into(),
            headers: vec![("X-Filename".into(), "shot.png".into())],
            body: Vec::new(),
            too_large: false,
        };
        assert_eq!(request.header("x-filename"), Some("shot.png"));
        assert_eq!(request.header("missing"), None);
    }

    #[test]
    fn decodes_percent_and_plus() {
        assert_eq!(percent_decode("a%2Fb+c"), "a/b c");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
