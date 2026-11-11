//! Real wgpu 30 render pass -> `wasi:webgpu` -> `soil:present` -> Canvas frame.
//!
//! Requires a native GPU adapter. Run with:
//!   cargo test -p soil-webgpu --test wgpu_test -- --ignored --nocapture

#[test]
#[ignore = "requires a native GPU adapter"]
fn wgpu_30_triangle_reaches_compositor() {
    let component = include_bytes!("../fixtures/wgpu-test.wasm");
    let (status, frame) =
        soil_webgpu::run_component_capture(component, 480, 640).expect("guest run");
    assert_eq!(status, 0, "compositor guest reported failure");
    let frame = frame.expect("compositor present");
    assert_eq!((frame.width, frame.height), (480, 640));
    assert_eq!(frame.rgba.len(), 480 * 640 * 4);

    // The triangle covers the centre; the corners are the clear colour.
    let pixel = |x: u32, y: u32| {
        let i = ((y * frame.width + x) * 4) as usize;
        &frame.rgba[i..i + 4]
    };
    let centre = pixel(240, 320);
    assert!(centre[0] > 200, "centre red: {centre:?}");
    assert!(centre[1] < 40 && centre[2] < 40, "centre red: {centre:?}");
    let corner = pixel(0, 0);
    assert!(corner[0] < 30 && corner[1] < 30, "corner: {corner:?}");
    assert_eq!(corner[3], 255);
}
