//! Run one component that imports `wasi:audio` and exports `run`.
//!
//! Usage: wasi-audio <component.wasm>

use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: wasi-audio <component.wasm>");
        return ExitCode::FAILURE;
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    match wasi_audio_wasmtime::run_component_blocking(&bytes) {
        Ok(0) => {
            println!("audio: ok");
            ExitCode::SUCCESS
        }
        Ok(status) => {
            eprintln!("audio: guest status {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("audio: {error:#}");
            ExitCode::FAILURE
        }
    }
}
