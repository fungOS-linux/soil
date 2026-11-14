//! A Spore that renders its own pixels presents them as bytes, not JSON.

use soil_runtime::{DEFAULT_FUEL, GuestScene, canvas::PIXEL_GUEST};

#[test]
fn guest_supplied_rgba_reaches_the_host_frame() {
    let mut guest = GuestScene::with_canvas(PIXEL_GUEST, DEFAULT_FUEL, None).unwrap();
    let frame = guest.render_canvas().unwrap();

    // Extent is the host's choice, even with no owner surface granted.
    assert_eq!((frame.width, frame.height), (480, 640));
    assert_eq!(frame.pixels.len(), 480 * 640 * 4);
    assert!(frame.pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));

    // Red ramps across row 0, so a wrong stride or a transposed copy shows up.
    assert_eq!(&frame.pixels[..4], &[0, 32, 200, 255]);
    let last_of_row = (479 * 4) as usize;
    assert_eq!(frame.pixels[last_of_row], (479u32 % 256) as u8);
    // Every later row is a byte copy of row 0.
    let last_row = (480 * 639 * 4) as usize;
    assert_eq!(&frame.pixels[last_row..last_row + 4], &[0, 32, 200, 255]);

    std::fs::create_dir_all("../../target/tmp").ok();
    let mut ppm = Vec::new();
    ppm.extend_from_slice(b"P6\n480 640\n255\n");
    for pixel in frame.pixels.as_chunks::<4>().0 {
        ppm.extend_from_slice(&pixel[..3]);
    }
    std::fs::write("../../target/tmp/soil-present-rgba.ppm", ppm).unwrap();
}

#[test]
fn a_short_frame_is_refused() {
    // The guest must fill exactly the host's extent; 4 bytes short is invalid.
    let short = wat::parse_str(
        r#"(module
             (import "soil:canvas/surface@0.1.0" "present_rgba"
              (func $p (param i32 i32) (result i32)))
             (memory (export "memory") 8)
             (func (export "run") (result i32) (call $p (i32.const 0) (i32.const 1228796))))"#,
    )
    .unwrap();
    let mut guest = GuestScene::with_canvas(&short, DEFAULT_FUEL, None).unwrap();
    // `{:#}` keeps the whole chain: the trap wrapper hides the reason.
    let error = format!("{:#}", guest.render_canvas().unwrap_err());
    assert!(error.contains("1228796"), "unexpected error: {error}");
}
