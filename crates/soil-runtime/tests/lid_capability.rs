mod common;

use common::socket_path;

use fungos_device_hal::{
    lid::{LidFailure, LidPosition, LidSample},
    lid_transport::{REQUEST, encode_response},
};
use soil_runtime::{
    DEFAULT_FUEL, GuestScene,
    lid::{LID_GUEST, LidGrant},
};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    time::Duration,
};

fn serve(
    listener: UnixListener,
    sample: Result<LidSample, LidFailure>,
    now: u64,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        assert_eq!(request, format!("{REQUEST}\n"));
        stream
            .write_all(encode_response(sample, now).as_bytes())
            .unwrap();
    })
}
fn sample(position: LidPosition) -> LidSample {
    LidSample {
        switch_asserted: position == LidPosition::Closed,
        physical_position: position,
        event_node: PathBuf::from("/dev/input/event1"),
        observed_monotonic_ms: 100,
    }
}
fn read(guest: &mut GuestScene) -> Vec<soil_runtime::lid::Observation> {
    assert_eq!(guest.render().unwrap().pixels.len(), 480 * 640 * 4);
    guest.take_lid_logs()
}
fn socket(name: &str) -> PathBuf {
    socket_path(&format!("lid-{name}.sock"))
}
fn guest(path: &Path) -> GuestScene {
    GuestScene::with_lid(
        LID_GUEST,
        DEFAULT_FUEL,
        true,
        Some(LidGrant {
            socket: path.into(),
            max_age: Duration::from_millis(1000),
        }),
    )
    .unwrap()
}
#[test]
fn actual_guest_initial_live_unknown_disconnect_reconnect() {
    let path = socket("lifecycle");
    let _ = std::fs::remove_file(&path);
    let mut guest = guest(&path);
    // Initial state is guest-read without requiring motion.
    let server = serve(
        UnixListener::bind(&path).unwrap(),
        Ok(sample(LidPosition::Open)),
        100,
    );
    let logs = read(&mut guest);
    server.join().unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].position, Some(LidPosition::Open));
    assert_eq!(logs[0].event_node, PathBuf::from("/dev/input/event1"));
    assert_eq!(logs[0].provider_monotonic_ms, 100);
    assert!(logs[0].age_ms <= 1000);
    std::fs::remove_file(&path).unwrap();
    let server = serve(
        UnixListener::bind(&path).unwrap(),
        Ok(sample(LidPosition::Closed)),
        100,
    );
    assert_eq!(read(&mut guest)[0].position, Some(LidPosition::Closed));
    server.join().unwrap();
    // Provider disappears. No retained closed value or fresh timestamp is exposed.
    std::fs::remove_file(&path).unwrap();
    let failure = read(&mut guest).remove(0);
    assert!(failure.failure.is_some());
    assert_eq!(failure.position, None);
    assert_eq!(failure.raw, None);
    assert!(read(&mut guest).is_empty()); // guest suppresses unchanged failure
    // New provider connection after restart, no host or guest restart.
    let server = serve(
        UnixListener::bind(&path).unwrap(),
        Ok(sample(LidPosition::Open)),
        100,
    );
    assert_eq!(read(&mut guest)[0].position, Some(LidPosition::Open));
    server.join().unwrap();
    std::fs::remove_file(&path).unwrap();
    let server = serve(
        UnixListener::bind(&path).unwrap(),
        Ok(sample(LidPosition::Open)),
        5000,
    );
    let stale = read(&mut guest).remove(0);
    server.join().unwrap();
    assert_eq!(stale.failure, Some(LidFailure::Stale));
    assert_eq!(stale.position, None);
    std::fs::remove_file(&path).unwrap();
    let server = serve(
        UnixListener::bind(&path).unwrap(),
        Ok(sample(LidPosition::Unqualified)),
        100,
    );
    assert_eq!(read(&mut guest)[0].position, Some(LidPosition::Unqualified));
    server.join().unwrap();
    std::fs::remove_file(&path).unwrap();
}
#[test]
fn initial_closed_without_motion_and_provider_failures() {
    let path = socket("closed");
    let _ = std::fs::remove_file(&path);
    let mut guest = guest(&path);
    for outcome in [
        Ok(sample(LidPosition::Closed)),
        Err(LidFailure::Resynchronizing),
        Err(LidFailure::Denied),
    ] {
        let server = serve(UnixListener::bind(&path).unwrap(), outcome.clone(), 100);
        let log = read(&mut guest).remove(0);
        server.join().unwrap();
        match outcome {
            Ok(sample) => assert_eq!(log.position, Some(sample.physical_position)),
            Err(e) => assert_eq!(log.failure, Some(e)),
        }
        std::fs::remove_file(&path).unwrap();
    }
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        drop(stream);
    });
    let log = read(&mut guest).remove(0);
    assert!(log.failure.is_some());
    assert_eq!(log.position, None);
    server.join().unwrap();
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn denied_guest_never_contacts_provider_and_default_stays_import_free() {
    let path = socket("denied");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut denied = GuestScene::with_lid(LID_GUEST, DEFAULT_FUEL, true, None).unwrap();
    assert_eq!(read(&mut denied)[0].failure, Some(LidFailure::Denied));
    assert!(read(&mut denied).is_empty());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(GuestScene::new(LID_GUEST, DEFAULT_FUEL).is_err());
    std::fs::remove_file(&path).unwrap();
}
#[test]
fn capability_cannot_import_arbitrary_host_functions_or_write_oob() {
    for wat in [
        "(module (import \"wasi_snapshot_preview1\" \"fd_write\" (func)))",
        "(module (import \"soil:device/lid@0.1.0\" \"read\" (func $r (param i32 i32) (result i32))) (memory (export \"memory\") 1) (func $s (drop (call $r (i32.const 65535) (i32.const 288)))) (start $s))",
    ] {
        let bytes = wat::parse_str(wat).unwrap();
        assert!(GuestScene::with_lid(&bytes, DEFAULT_FUEL, true, None).is_err());
    }
}
