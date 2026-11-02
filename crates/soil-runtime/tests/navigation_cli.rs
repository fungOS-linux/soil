//! Combined CLI grants use real battery Wasm + actual owner; no input bypass.
use fungos_device_hal::{
    keypad::KeypadRing,
    keypad_transport::{decode_request, encode_response},
};
use soil_runtime::battery::BATTERY_CANVAS_GUEST;
use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    process::Command,
};
#[test]
fn combined_battery_simple_jarvis_keypad_cli_and_passive_outer() {
    let dir = std::env::temp_dir().join(format!("soil-nav-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let guest = dir.join("battery.wasm");
    std::fs::write(&guest, BATTERY_CANVAS_GUEST).unwrap();
    let socket = dir.join("keys.sock");
    let output = dir.join("frame.bgr");
    let listener = UnixListener::bind(&socket).unwrap();
    let thread = std::thread::spawn(move || {
        let mut ring = KeypadRing::new(17);
        for i in 0..2 {
            let (mut s, _) = listener.accept().unwrap();
            let mut q = String::new();
            s.read_to_string(&mut q).unwrap();
            if i == 1 {
                ring.input(1, 106, 1, 0);
                ring.input(1, 106, 0, 0);
            }
            s.write_all(encode_response(&ring.poll(decode_request(&q).unwrap(), 0), 0).as_bytes())
                .unwrap();
        }
    });
    let result = Command::new(env!("CARGO_BIN_EXE_soil-canvas"))
        .args([
            "--guest",
            guest.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--frames",
            "2",
            "--interval-ms",
            "50",
            "--ui",
            "simple-jarvis",
            "--allow-battery",
            "--keypad-socket",
            socket.to_str().unwrap(),
            "--battery-socket",
            "/nonexistent",
            "--allow-keypad",
            "--surface",
            "inner",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("normalized=Right guest=input canvas=owner")
    );
    assert_eq!(std::fs::metadata(&output).unwrap().len(), 921600);
    thread.join().unwrap();
    // Denied grants don't connect even when a socket is listening.
    let listener = UnixListener::bind(dir.join("denied.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_soil-canvas"))
        .args([
            "--guest",
            guest.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--frames",
            "1",
            "--interval-ms",
            "50",
            "--keypad-socket",
            dir.join("denied.sock").to_str().unwrap(),
            "--deny-keypad",
            "--deny-battery",
            "--ui",
            "simple-jarvis",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(listener.accept().is_err());
    let result = Command::new(env!("CARGO_BIN_EXE_soil-canvas"))
        .args([
            "--guest",
            guest.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--frames",
            "1",
            "--interval-ms",
            "50",
            "--keypad-socket",
            dir.join("denied.sock").to_str().unwrap(),
            "--allow-keypad",
            "--deny-battery",
            "--ui",
            "simple-jarvis",
            "--surface",
            "outer",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("outer surface is passive"));
    assert!(listener.accept().is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
