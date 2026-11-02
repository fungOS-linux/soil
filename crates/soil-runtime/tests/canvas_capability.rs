use soil_runtime::{
    DEFAULT_FUEL, GuestScene, Key,
    canvas::{CANVAS_GUEST, OwnerSurface, SurfaceSize},
};

#[test]
fn simple_jarvis_uses_granted_canvas_owner_at_both_extents() {
    for size in [SurfaceSize::Inner, SurfaceSize::Outer] {
        let mut guest = GuestScene::with_canvas(
            CANVAS_GUEST,
            DEFAULT_FUEL,
            Some(Box::new(OwnerSurface::simple_jarvis(size).unwrap())),
        )
        .unwrap();
        let initial = guest.render_canvas().unwrap();
        assert_eq!((initial.width, initial.height), size.dimensions());
        assert_eq!(
            initial.to_bgr24().unwrap().len(),
            (initial.width * initial.height * 3) as usize
        );
        assert!(
            initial
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 255)
        );
        guest.key(Key::Select).unwrap();
        assert_ne!(initial.pixels, guest.render_canvas().unwrap().pixels);
        guest.key(Key::Back).unwrap();
        assert_eq!(initial.pixels, guest.render_canvas().unwrap().pixels);
    }
}

#[test]
fn real_guest_calls_actual_canvas_owner_and_navigation() {
    actual_owner_at_size(SurfaceSize::Inner);
}
#[test]
fn actual_outer_owner_pixels_and_navigation() {
    actual_owner_at_size(SurfaceSize::Outer);
}
fn actual_owner_at_size(size: SurfaceSize) {
    let (width, height) = size.dimensions();
    let mut guest = GuestScene::with_canvas(
        CANVAS_GUEST,
        DEFAULT_FUEL,
        Some(Box::new(OwnerSurface::with_size(size).unwrap())),
    )
    .unwrap();
    let initial = guest.render_canvas().unwrap();
    assert_eq!((initial.width, initial.height), (width, height));
    assert_eq!(initial.pixels.len(), (width * height * 4) as usize);
    assert_eq!(
        initial.to_bgr24().unwrap().len(),
        (width * height * 3) as usize
    );
    assert!(
        initial
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[3] == 255)
    );
    assert!(
        initial
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p != &initial.pixels[..4])
    );
    guest.key(Key::Down).unwrap();
    let moved = guest.render_canvas().unwrap();
    assert_ne!(initial.pixels, moved.pixels);
    guest.key(Key::Select).unwrap();
    let detail = guest.render_canvas().unwrap();
    assert_ne!(moved.pixels, detail.pixels);
    guest.key(Key::Back).unwrap();
    assert_ne!(detail.pixels, guest.render_canvas().unwrap().pixels);
}
#[test]
fn rejects_owner_frames_that_do_not_match_granted_extent() {
    use soil_runtime::canvas::CanvasSurface;
    struct WrongExtent;
    impl CanvasSurface for WrongExtent {
        fn size(&self) -> SurfaceSize {
            SurfaceSize::Outer
        }
        fn present(&mut self) -> anyhow::Result<fpl_gfx::SoftwareImage> {
            Ok(fpl_gfx::SoftwareImage {
                width: 480,
                height: 640,
                pixels: vec![255; 480 * 640 * 4],
            })
        }
        fn key(&mut self, _: Key) -> anyhow::Result<()> {
            Ok(())
        }
    }
    let mut guest =
        GuestScene::with_canvas(CANVAS_GUEST, DEFAULT_FUEL, Some(Box::new(WrongExtent))).unwrap();
    assert!(
        format!("{:#}", guest.render_canvas().unwrap_err()).contains("invalid Canvas owner frame")
    );
}

#[test]
fn canvas_permission_denied_and_no_cross_capability_grants() {
    let mut guest = GuestScene::with_canvas(CANVAS_GUEST, DEFAULT_FUEL, None).unwrap();
    assert!(
        guest
            .render_canvas()
            .unwrap_err()
            .to_string()
            .contains("status 1")
    );
    assert!(GuestScene::new(CANVAS_GUEST, DEFAULT_FUEL).is_err());
    assert!(GuestScene::with_canvas(soil_runtime::lid::LID_GUEST, DEFAULT_FUEL, None).is_err());
    assert!(GuestScene::with_lid(CANVAS_GUEST, DEFAULT_FUEL, true, None).is_err());
}
