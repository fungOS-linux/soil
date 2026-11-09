mod common;

use anyhow::Result;
use common::socket_path;
use fpl_gfx::SoftwareImage;
use fungos_device_hal::{
    battery_transport::{BatteryFailure, encode_response},
    services::{BatteryProvenance, BatterySample, ChargingState},
};
use s22_canvas_surface::{Availability, BatteryView};
use soil_runtime::{
    DEFAULT_FUEL, GuestScene, Key,
    battery::{BATTERY_CANVAS_GUEST, BatteryGrant},
    canvas::{CANVAS_GUEST, CanvasSurface},
};
use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Surface(Arc<Mutex<Vec<BatteryView>>>);
impl CanvasSurface for Surface {
    fn present(&mut self) -> Result<SoftwareImage> {
        Ok(SoftwareImage {
            width: 480,
            height: 640,
            pixels: vec![255; 480 * 640 * 4],
        })
    }
    fn key(&mut self, _: Key) -> Result<()> {
        Ok(())
    }
    fn update_status(&mut self, v: BatteryView) -> Result<()> {
        self.0.lock().unwrap().push(v);
        Ok(())
    }
}
fn sample(percent: Option<u8>) -> BatterySample {
    BatterySample {
        percent,
        charging: ChargingState::Unknown,
        voltage_microvolts: Some(3885732),
        current_microamps: None,
        temperature_deci_celsius: None,
        provenance: BatteryProvenance::LinuxPowerSupply("/sys/class/power_supply/test".into()),
        observed_monotonic_ms: 100,
    }
}
fn fixture(
    wires: Vec<String>,
    max_age: Duration,
) -> (
    GuestScene,
    Arc<Mutex<Vec<BatteryView>>>,
    std::thread::JoinHandle<()>,
) {
    fixture_guest(wires, max_age, BATTERY_CANVAS_GUEST)
}
fn fixture_guest(
    wires: Vec<String>,
    max_age: Duration,
    bytes: &[u8],
) -> (
    GuestScene,
    Arc<Mutex<Vec<BatteryView>>>,
    std::thread::JoinHandle<()>,
) {
    static SERIAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = socket_path(&format!(
        "battery-{}",
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let remove = path.clone();
    let thread = std::thread::spawn(move || {
        for wire in wires {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            assert_eq!(request, "battery-v1\n");
            stream.write_all(wire.as_bytes()).unwrap();
        }
        std::fs::remove_file(remove).unwrap();
    });
    let views = Arc::new(Mutex::new(vec![]));
    let guest = GuestScene::with_battery_canvas(
        bytes,
        DEFAULT_FUEL,
        Some(Box::new(Surface(views.clone()))),
        Some(BatteryGrant::new(path, max_age).with_refresh(Duration::ZERO)),
    )
    .unwrap();
    (guest, views, thread)
}
#[test]
fn guest_numeric_tampering_is_ignored_and_tokens_expire_between_frames() {
    let wasm = wat::parse_str(
        r#"(module
      (import "soil:device/battery@0.1.0" "read" (func $r (param i32 i32) (result i32)))
      (import "soil:device/battery@0.1.0" "update-status" (func $u (param i64) (result i32)))
      (import "soil:canvas/surface@0.1.0" "present" (func $p (result i32)))
      (memory (export "memory") 1)
      (global $runs (mut i32) (i32.const 0))
      (func (export "run") (result i32)
       (if (i32.eqz (global.get $runs)) (then
        (drop (call $r (i32.const 0) (i32.const 24)))
        (i32.store (i32.const 16) (i32.const 99))
        (i32.store (i32.const 20) (i32.const 1))))
       (global.set $runs (i32.const 1))
       (drop (call $u (i64.load (i32.const 0))))
       (call $p)))"#,
    )
    .unwrap();
    let (mut guest, views, thread) = fixture_guest(
        vec![encode_response(Ok(sample(None)), 101)],
        Duration::from_secs(1),
        &wasm,
    );
    guest.render_canvas().unwrap();
    thread.join().unwrap();
    assert_eq!(views.lock().unwrap()[1].percent, None);
    assert_eq!(views.lock().unwrap()[1].voltage_uv, Some(3885732));
    assert!(guest.render_canvas().is_err());
}

#[test]
fn battery_guest_renders_actual_owner_both_extents() {
    use soil_runtime::canvas::{OwnerSurface, SurfaceSize};
    for size in [SurfaceSize::Inner, SurfaceSize::Outer] {
        let owner = OwnerSurface::simple_jarvis(size).unwrap();
        let mut guest = GuestScene::with_battery_canvas(
            BATTERY_CANVAS_GUEST,
            DEFAULT_FUEL,
            Some(Box::new(owner)),
            None,
        )
        .unwrap();
        let frame = guest.render_canvas().unwrap();
        assert_eq!((frame.width, frame.height), size.dimensions());
        assert!(frame.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        assert!(
            frame
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[..3] != frame.pixels[..3])
        );
    }
}

#[test]
fn actual_guest_unknown_zero_failures_recovery() {
    let wires = vec![
        encode_response(Ok(sample(None)), 101),
        encode_response(Ok(sample(Some(0))), 101),
        encode_response(Err(BatteryFailure::Denied), 101),
        "garbage".into(),
        String::new(),
        encode_response(Ok(sample(Some(42))), 101),
    ];
    let (mut guest, views, thread) = fixture(wires, Duration::from_secs(1));
    for _ in 0..6 {
        guest.render_canvas().unwrap();
    }
    thread.join().unwrap();
    let v = views.lock().unwrap();
    let v: Vec<_> = v.chunks(2).map(|v| v[1]).collect();
    assert_eq!(v[0].percent, None);
    assert_eq!(v[0].voltage_uv, Some(3885732));
    assert_eq!(v[0].availability, Availability::Fresh);
    assert_eq!(v[1].percent, Some(0));
    assert_eq!(v[2].availability, Availability::Denied);
    for view in &v[2..5] {
        assert_eq!(view.percent, None);
        assert_eq!(view.voltage_uv, None);
    }
    assert_eq!(v[5].percent, Some(42));
}
#[test]
fn stale_and_retention_revalidation() {
    let (mut guest, views, thread) = fixture(
        vec![encode_response(Ok(sample(Some(42))), 101)],
        Duration::ZERO,
    );
    guest.render_canvas().unwrap();
    thread.join().unwrap();
    assert!(
        views
            .lock()
            .unwrap()
            .iter()
            .all(|v| v.availability == Availability::Stale
                && v.percent.is_none()
                && v.voltage_uv.is_none())
    );
}
#[test]
fn denied_no_socket_and_default_backcompat() {
    let path = socket_path("battery-denied");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let views = Arc::new(Mutex::new(vec![]));
    let mut guest = GuestScene::with_battery_canvas(
        BATTERY_CANVAS_GUEST,
        DEFAULT_FUEL,
        Some(Box::new(Surface(views.clone()))),
        None,
    )
    .unwrap();
    guest.render_canvas().unwrap();
    assert_eq!(views.lock().unwrap()[0].availability, Availability::Denied);
    assert!(listener.accept().is_err());
    std::fs::remove_file(path).unwrap();
    assert!(GuestScene::with_canvas(BATTERY_CANVAS_GUEST, DEFAULT_FUEL, None).is_err());
    let mut old = GuestScene::with_canvas(
        CANVAS_GUEST,
        DEFAULT_FUEL,
        Some(Box::new(Surface(views.clone()))),
    )
    .unwrap();
    let count = views.lock().unwrap().len();
    old.render_canvas().unwrap();
    assert_eq!(views.lock().unwrap().len(), count);
}
fn adversary(body: &str) -> GuestScene {
    let wasm=wat::parse_str(format!("(module (import \"soil:device/battery@0.1.0\" \"read\" (func $r (param i32 i32) (result i32))) (import \"soil:device/battery@0.1.0\" \"update-status\" (func $u (param i64) (result i32))) (memory (export \"memory\") 1) (func (export \"run\") (result i32) {body} i32.const 0))")).unwrap();
    GuestScene::with_battery_canvas(&wasm, DEFAULT_FUEL, None, None).unwrap()
}
#[test]
fn adversarial_memory_tokens_and_budgets() {
    for body in [
        "i32.const 65530 i32.const 24 call $r drop",
        "i32.const 0 i32.const 23 call $r drop",
        "i64.const 1 call $u drop",
        "i32.const 0 i32.const 24 call $r drop i64.const 999 call $u drop",
        "i32.const 0 i32.const 24 call $r drop i32.const 0 i32.const 24 call $r drop",
        "i32.const 0 i32.const 24 call $r drop i64.const 1 call $u drop i64.const 1 call $u drop",
    ] {
        assert!(adversary(body).render_canvas().is_err(), "{body}");
    }
}
