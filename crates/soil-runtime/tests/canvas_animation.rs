use soil_runtime::{
    DEFAULT_FUEL, GuestScene, Key,
    battery::BATTERY_CANVAS_GUEST,
    canvas::{ANIMATED_BATTERY_GUEST, OwnerSurface, SurfaceSize},
};
#[test]
fn actual_guest_ticks_deterministic_owner_both_extents_and_navigation() {
    for size in [SurfaceSize::Inner, SurfaceSize::Outer] {
        let mut guest = GuestScene::with_animated_battery_canvas(
            ANIMATED_BATTERY_GUEST,
            DEFAULT_FUEL,
            Some(Box::new(OwnerSurface::simple_jarvis(size).unwrap())),
            None,
        )
        .unwrap();
        guest.tick(0).unwrap();
        let first = guest.render_canvas().unwrap();
        guest.tick(0).unwrap();
        assert_eq!(first, guest.render_canvas().unwrap());
        guest.tick(79).unwrap();
        assert_eq!(first, guest.render_canvas().unwrap());
        guest.tick(800).unwrap();
        let moved = guest.render_canvas().unwrap();
        assert_ne!(first, moved);
        guest.tick(800).unwrap();
        assert_eq!(moved, guest.render_canvas().unwrap());
        guest.key(Key::Right).unwrap();
        guest.tick(800).unwrap();
        let battery = guest.render_canvas().unwrap();
        guest.tick(1600).unwrap();
        assert_eq!(battery, guest.render_canvas().unwrap());
        guest.key(Key::Home).unwrap();
        guest.tick(0).unwrap();
        assert_eq!(first, guest.render_canvas().unwrap());
    }
}
#[test]
fn animation_separate_permission_and_host_time_cannot_be_forged() {
    assert!(
        GuestScene::with_battery_canvas(ANIMATED_BATTERY_GUEST, DEFAULT_FUEL, None, None).is_err()
    );
    let mut legacy =
        GuestScene::with_battery_canvas(BATTERY_CANVAS_GUEST, DEFAULT_FUEL, None, None).unwrap();
    assert!(legacy.tick(0).is_err());
    for body in [
        "(drop (call $t (i64.const 999)))",
        "(drop (call $t (local.get 0))) (drop (call $t (local.get 0)))",
        "",
    ] {
        let wasm=wat::parse_str(format!(r#"(module (import "soil:canvas/animation@0.1.0" "tick" (func $t (param i64) (result i32))) (func (export "tick") (param i64) {body}))"#)).unwrap();
        let mut guest =
            GuestScene::with_animated_battery_canvas(&wasm, DEFAULT_FUEL, None, None).unwrap();
        assert!(guest.tick(80).is_err());
    }
}
