//! Real guest -> `wasi:webgpu` -> wgpu-core compute proof.
//!
//! This requires a GPU adapter on the host. Run explicitly with:
//!   cargo test -p soil-webgpu --test hello_compute -- --ignored --nocapture

#[test]
#[ignore = "requires a native GPU adapter"]
fn guest_collatz_compute_returns_zero() {
    let component = include_bytes!("../fixtures/hello-compute.wasm");
    let status = soil_webgpu::run_component_blocking(component).expect("guest run");
    assert_eq!(status, 0, "guest reported WebGPU compute failure");
}
