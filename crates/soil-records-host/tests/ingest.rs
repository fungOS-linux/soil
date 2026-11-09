use std::{
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use fungos_records::Store;

#[test]
fn ingest_writes_a_schema_tagged_record() {
    let root = tempfile::tempdir().unwrap();
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);

    let mut child = Command::new(env!("CARGO_BIN_EXE_soil-records-ingest"))
        .arg("--root")
        .arg(root.path())
        .arg("--book")
        .arg("personal/test/crm")
        .arg("--schema-prefix")
        .arg("fungos.crm/")
        .arg("--token")
        .arg("test-token")
        .arg("--listen")
        .arg(address.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    for _ in 0..100 {
        if TcpStream::connect(address).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }

    let body = r#"{"book":"personal/test/crm","id":"a1","schema":"fungos.crm/account@1","body":{"name":"Acme"}}"#;
    let request = format!(
        "POST /api/records HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"ok\":true"), "{response}");

    let store = Store::open(root.path(), &root.path().join("device_id")).unwrap();
    let record = store.get("personal/test/crm", "a1").unwrap();
    assert_eq!(record.schema_id, "fungos.crm/account@1");
    assert_eq!(record.fields["name"], "Acme");

    let _ = child.kill();
    let _ = child.wait();
}
