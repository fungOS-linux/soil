//! Talking to `soil-termd`.
//!
//! One JSON object per connection, one back, then close. The daemon owns the
//! ptys; this process is a front that can restart without taking the shells with
//! it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

pub fn call(socket: &Path, request: &Value) -> Result<Value, String> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|error| format!("connect {}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    let mut body = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    body.push(b'\n');
    stream.write_all(&body).map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(line.trim()).map_err(|error| error.to_string())?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    Ok(value)
}

/// The session a request means: the one it names, or the first live one.
pub fn pick(socket: &Path, wanted: Option<u64>) -> Result<Option<Value>, String> {
    let list = call(socket, &json!({ "op": "list" }))?;
    let sessions = list
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(id) = wanted
        && let Some(found) = sessions.iter().find(|item| item.get("id").and_then(Value::as_u64) == Some(id))
    {
        return Ok(Some(found.clone()));
    }
    Ok(sessions
        .into_iter()
        .find(|item| item.get("exited").and_then(Value::as_bool) != Some(true)))
}
