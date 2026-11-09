//! Existing HAL typed transport through host normalization, Wasm input import,
//! actual Canvas owner model and render. No physical device or fake UI.

mod common;

use common::socket_path;
use fungos_device_hal::{
    keypad::{KeypadResync, KeypadRing},
    keypad_transport::{decode_request, encode_response},
};
use soil_runtime::{
    DEFAULT_FUEL, GuestScene,
    canvas::{CANVAS_GUEST, OwnerSurface},
    keypad::KeypadGrant,
};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
};

#[test]
fn battery_guest_input_navigates_actual_simple_jarvis_owner() {
    use soil_runtime::{Key, battery::BATTERY_CANVAS_GUEST, canvas::SurfaceSize};
    for size in [SurfaceSize::Inner, SurfaceSize::Outer] {
        let mut guest = GuestScene::with_battery_canvas(
            BATTERY_CANVAS_GUEST,
            DEFAULT_FUEL,
            Some(Box::new(OwnerSurface::simple_jarvis(size).unwrap())),
            None,
        )
        .unwrap();
        let home = guest.render_canvas().unwrap();
        guest.key(Key::Right).unwrap();
        let battery = guest.render_canvas().unwrap();
        assert_ne!(home, battery);
        guest.key(Key::Right).unwrap();
        let device = guest.render_canvas().unwrap();
        assert_ne!(battery, device);
        guest.key(Key::Right).unwrap();
        assert_eq!(device, guest.render_canvas().unwrap()); // bounded, no wrap
        guest.key(Key::Back).unwrap();
        assert_eq!(home, guest.render_canvas().unwrap());
        guest.key(Key::Down).unwrap();
        let focus = guest.render_canvas().unwrap();
        assert_ne!(home, focus);
        guest.key(Key::Select).unwrap();
        assert_eq!(device, guest.render_canvas().unwrap());
        guest.key(Key::Back).unwrap();
        assert_eq!(home, guest.render_canvas().unwrap());
    }
}

#[test]
fn hal_socket_to_guest_to_canvas_and_provider_restart() {
    let path = socket_path(&format!("canvas-keys-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let thread = std::thread::spawn(move || {
        let mut ring = KeypadRing::new(42);
        for i in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            let cursor = decode_request(&request).unwrap();
            if i == 1 {
                ring.input(1, 108, 1, 0);
                ring.input(1, 108, 0, 0);
            }
            if i == 2 {
                ring.input(1, 28, 1, 0);
                ring.input(1, 28, 0, 0);
            }
            if i == 3 {
                ring = KeypadRing::new(43);
            }
            if i == 4 {
                ring.input(1, 158, 1, 0);
                ring.input(1, 158, 0, 0);
            }
            let batch = ring.poll(cursor, 0);
            if i == 3 {
                assert_eq!(batch.resync, Some(KeypadResync::Restart));
            }
            stream
                .write_all(encode_response(&batch, 0).as_bytes())
                .unwrap();
        }
    });
    let mut keys = KeypadGrant::new(Some(path.clone()));
    let mut guest = GuestScene::with_canvas(
        CANVAS_GUEST,
        DEFAULT_FUEL,
        Some(Box::new(OwnerSurface::new().unwrap())),
    )
    .unwrap();
    assert!(keys.poll().unwrap().is_empty()); // starts at head, no history
    let initial = guest.render_canvas().unwrap();
    for key in keys.poll().unwrap() {
        guest.key(key).unwrap();
    }
    let moved = guest.render_canvas().unwrap();
    assert_ne!(initial.pixels, moved.pixels);
    for key in keys.poll().unwrap() {
        guest.key(key).unwrap();
    }
    let detail = guest.render_canvas().unwrap();
    assert_ne!(moved.pixels, detail.pixels);
    assert!(keys.poll().unwrap().is_empty()); // restarted provider, no replay
    for key in keys.poll().unwrap() {
        guest.key(key).unwrap();
    }
    assert_ne!(detail.pixels, guest.render_canvas().unwrap().pixels);
    thread.join().unwrap();
    std::fs::remove_file(path).unwrap();
}
