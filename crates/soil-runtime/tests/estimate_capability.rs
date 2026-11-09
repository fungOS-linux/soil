//! All metadata and numeric values here are synthetic transport fixtures, not
//! model math/profile data or claims about live hardware.

mod common;

use anyhow::Result;
use common::socket_path;
use fpl_gfx::SoftwareImage;
use fungos_device_hal::estimate_transport::{EstimateFailure, TrustedEstimateReceipts};
use s22_canvas_surface::{Availability, ModelEstimateView};
use soil_runtime::{
    DEFAULT_FUEL, GuestScene, Key,
    canvas::{CANVAS_GUEST, CanvasSurface, OwnerSurface, SurfaceSize},
    estimate::{ESTIMATE_CANVAS_GUEST, EstimateGrant},
};
use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    sync::{Arc, Mutex},
    time::Duration,
};
fn receipt() -> TrustedEstimateReceipts {
    TrustedEstimateReceipts {
        profile_id: "synthetic-profile".into(),
        profile_sha256: "a".repeat(64),
        algorithm_id: "synthetic-algorithm".into(),
        algorithm_sha256: "b".repeat(64),
        ocv_qualification_id: "synthetic-ocv".into(),
        temperature_qualification_id: "synthetic-temp".into(),
    }
}
fn wire() -> String {
    let r = receipt();
    format!(
        "FUNGOS-ESTIMATE/1\tOK\t57\t{}\t{}\t{}\t{}\t{}\t{}\t0\t0\t100\t100\n",
        r.profile_id,
        r.profile_sha256,
        r.algorithm_id,
        r.algorithm_sha256,
        r.ocv_qualification_id,
        r.temperature_qualification_id
    )
}
struct Surface(Arc<Mutex<Vec<ModelEstimateView>>>);
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
    fn update_model_estimate(&mut self, v: ModelEstimateView) -> Result<()> {
        self.0.lock().unwrap().push(v);
        Ok(())
    }
}
fn fixture(
    wires: Vec<String>,
    bytes: &[u8],
    surface: Box<dyn CanvasSurface>,
    max_age: Duration,
) -> (GuestScene, std::thread::JoinHandle<()>) {
    static SERIAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = socket_path(&format!(
        "estimate-{}",
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let remove = path.clone();
    let thread = std::thread::spawn(move || {
        for wire in wires {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            assert_eq!(request, "estimate-v1\n");
            stream.write_all(wire.as_bytes()).unwrap();
        }
        std::fs::remove_file(remove).unwrap();
    });
    (
        GuestScene::with_estimate_canvas(
            bytes,
            DEFAULT_FUEL,
            Some(surface),
            Some(EstimateGrant::new(path, vec![receipt()], max_age).unwrap()),
        )
        .unwrap(),
        thread,
    )
}
#[test]
fn actual_wasm_typed_results_no_fallback_recovery() {
    let mut wires = vec![wire()];
    for reason in [
        "unavailable",
        "unqualified-ocv",
        "unqualified-temperature",
        "algorithm-unverified",
        "profile-unqualified",
    ] {
        wires.push(format!("FUNGOS-ESTIMATE/1\tUNKNOWN\t{reason}\n"));
    }
    for reason in [
        "denied",
        "backend",
        "disconnected",
        "protocol",
        "stale",
        "invalid-time",
        "untrusted-receipt",
        "overflow",
    ] {
        wires.push(format!("FUNGOS-ESTIMATE/1\tERR\t{reason}\n"));
    }
    wires.extend([
        wire().replace("synthetic-profile", "untrusted-profile"),
        String::new(),
        "invalid".into(),
        wire().replacen("\t57\t", "\t0\t", 1),
    ]);
    let count = wires.len();
    let views = Arc::new(Mutex::new(vec![]));
    let (mut guest, thread) = fixture(
        wires,
        ESTIMATE_CANVAS_GUEST,
        Box::new(Surface(views.clone())),
        Duration::from_secs(5),
    );
    for _ in 0..count {
        guest.render_canvas().unwrap();
    }
    thread.join().unwrap();
    let views = views.lock().unwrap();
    assert_eq!(views[2].availability, Availability::Unknown);
    assert_eq!(views[12].availability, Availability::Denied);
    assert_eq!(views[20].availability, Availability::Stale);
    let v: Vec<_> = views.chunks(2).map(|v| v[1]).collect();
    assert_eq!(v[0].percent, Some(57));
    assert_eq!(v[0].availability, Availability::Fresh);
    for view in &v[1..count - 1] {
        assert_eq!(view.percent, None);
        assert_ne!(view.availability, Availability::Fresh);
    }
    assert_eq!(v[6].availability, Availability::Denied);
    assert_eq!(v[10].availability, Availability::Stale);
    assert_eq!(v[count - 1].percent, Some(0));
}
#[test]
fn permission_and_import_separation_no_contact() {
    let path = socket_path("estimate-denied");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let views = Arc::new(Mutex::new(vec![]));
    let mut guest = GuestScene::with_estimate_canvas(
        ESTIMATE_CANVAS_GUEST,
        DEFAULT_FUEL,
        Some(Box::new(Surface(views.clone()))),
        None,
    )
    .unwrap();
    guest.render_canvas().unwrap();
    assert_eq!(views.lock().unwrap()[0].availability, Availability::Denied);
    assert!(listener.accept().is_err());
    std::fs::remove_file(path).unwrap();
    assert!(GuestScene::with_canvas(ESTIMATE_CANVAS_GUEST, DEFAULT_FUEL, None).is_err());
    assert!(
        GuestScene::with_battery_canvas(ESTIMATE_CANVAS_GUEST, DEFAULT_FUEL, None, None).is_err()
    );
    assert!(matches!(
        EstimateGrant::new("/unused".into(), vec![], Duration::from_secs(1)),
        Err(EstimateFailure::Protocol)
    ));
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
fn wasm(body: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module
 (import "soil:device/estimate@0.1.0" "read" (func $r (param i32 i32) (result i32)))
 (import "soil:device/estimate@0.1.0" "update-status" (func $u (param i64) (result i32)))
 (import "soil:canvas/surface@0.1.0" "present" (func $p (result i32)))
 (memory (export "memory") 1)
 (global $runs (mut i32) (i32.const 0))
 (func (export "run") (result i32) {body}))"#
    ))
    .unwrap()
}
#[test]
fn malicious_guest_memory_budget_token_replay() {
    for body in [
        "i32.const 65530 i32.const 24 call $r drop i32.const 0",
        "i32.const 0 i32.const 23 call $r drop i32.const 0",
        "i64.const 1 call $u",
        "i32.const 0 i32.const 24 call $r drop i64.const 999 call $u",
        "i32.const 0 i32.const 24 call $r drop i32.const 0 i32.const 24 call $r",
        "i32.const 0 i32.const 24 call $r drop i64.const 1 call $u drop i64.const 1 call $u",
    ] {
        let mut guest =
            GuestScene::with_estimate_canvas(&wasm(body), DEFAULT_FUEL, None, None).unwrap();
        assert!(guest.render_canvas().is_err(), "{body}");
    }
    let bytes = wasm(
        r#"(if (i32.eqz (global.get $runs)) (then
 (drop (call $r (i32.const 0) (i32.const 24)))
 (i32.store (i32.const 8) (i32.const 3))
 (i32.store (i32.const 16) (i32.const 99))))
 (global.set $runs (i32.const 1))
 (drop (call $u (i64.load (i32.const 0)))) (call $p)"#,
    );
    let views = Arc::new(Mutex::new(vec![]));
    let double_read =
        wasm("i32.const 0 i32.const 24 call $r drop i32.const 0 i32.const 24 call $r");
    let (mut bounded, transport) = fixture(
        vec![wire()],
        &double_read,
        Box::new(Surface(views.clone())),
        Duration::from_secs(5),
    );
    assert!(bounded.render_canvas().is_err());
    transport.join().unwrap();
    assert!(views.lock().unwrap().is_empty());
    let (mut guest, thread) = fixture(
        vec![wire()],
        &bytes,
        Box::new(Surface(views.clone())),
        Duration::from_secs(5),
    );
    guest.render_canvas().unwrap();
    thread.join().unwrap();
    assert_eq!(views.lock().unwrap()[1].percent, Some(57));
    assert_eq!(views.lock().unwrap()[1].availability, Availability::Fresh);
    assert!(guest.render_canvas().is_err());
}
#[test]
fn actual_owner_native_pixels_fresh_and_numberless() {
    for size in [SurfaceSize::Inner, SurfaceSize::Outer] {
        let owner = OwnerSurface::simple_jarvis(size).unwrap();
        let (mut guest, thread) = fixture(
            vec![
                wire(),
                "FUNGOS-ESTIMATE/1\tUNKNOWN\tunqualified-ocv\n".into(),
                "FUNGOS-ESTIMATE/1\tERR\tstale\n".into(),
                "FUNGOS-ESTIMATE/1\tERR\tdenied\n".into(),
            ],
            ESTIMATE_CANVAS_GUEST,
            Box::new(owner),
            Duration::from_secs(5),
        );
        guest.key(Key::Right).unwrap(); // Estimate belongs to Battery, not Home menu.
        let fresh = guest.render_canvas().unwrap();
        assert_eq!((fresh.width, fresh.height), size.dimensions());
        let mut labels = Vec::new();
        fpl_gfx::status_widgets::model_estimate::visit(
            ModelEstimateView {
                percent: Some(57),
                availability: Availability::Fresh,
            },
            |mark| labels.push(mark),
        )
        .unwrap();
        assert!(labels.iter().any(|m| matches!(m, fpl_gfx::status_widgets::Mark::Text {text,..} if text == "Estimated 57%")));
        assert!(labels.iter().any(|m| matches!(m, fpl_gfx::status_widgets::Mark::Text {text,..} if text == "Model estimate")));
        let mut expected = OwnerSurface::simple_jarvis(size).unwrap();
        expected.key(Key::Right).unwrap();
        expected
            .update_model_estimate(ModelEstimateView {
                percent: Some(57),
                availability: Availability::Fresh,
            })
            .unwrap();
        assert_eq!(fresh, expected.present().unwrap());
        for availability in [
            Availability::Unknown,
            Availability::Stale,
            Availability::Denied,
        ] {
            let hidden = guest.render_canvas().unwrap();
            assert_ne!(fresh, hidden);
            expected
                .update_model_estimate(ModelEstimateView {
                    percent: None,
                    availability,
                })
                .unwrap();
            assert_eq!(hidden, expected.present().unwrap());
            expected
                .update_model_estimate(ModelEstimateView {
                    percent: Some(57),
                    availability,
                })
                .unwrap();
            assert_eq!(
                hidden,
                expected.present().unwrap(),
                "cached numeric must not affect pixels"
            );
            assert!(hidden.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        }
        thread.join().unwrap();
    }
}
