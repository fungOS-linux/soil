//! Run one component that imports `wasi:webgpu` and exports `run`.
//!
//! Usage: soil-webgpu <component.wasm>

use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: soil-webgpu <component.wasm>");
        return ExitCode::FAILURE;
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    match soil_webgpu::run_component_blocking(&bytes) {
        Ok(0) => {
            println!("webgpu: ok");
            ExitCode::SUCCESS
        }
        Ok(status) => {
            eprintln!("webgpu: guest status {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("webgpu: {error:#}");
            ExitCode::FAILURE
        }
    }
}
