//! A real wgpu 29 program -> `wasi:webgpu` -> wgpu-core compute proof.
//!
//! This requires a GPU adapter on the host. Run explicitly with:
//!   cargo test -p soil-webgpu --test wgpu_compute -- --ignored --nocapture

#[test]
#[ignore = "requires a native GPU adapter"]
fn wgpu_29_collatz_compute_returns_zero() {
    let component = include_bytes!("../fixtures/wgpu-compute.wasm");
    let status = soil_webgpu::run_component_blocking(component).expect("guest run");
    assert_eq!(status, 0, "wgpu guest reported failure");
}
