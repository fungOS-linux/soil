//! Run a cpal-shaped Rust program compiled to `wasm32-wasip1-threads`.
//!
//! Usage: soil-cpal <program.wasm>

use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: soil-cpal <program.wasm>");
        return ExitCode::FAILURE;
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("soil-cpal: read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match soil_process::ProcessRuntime::new(&bytes) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("soil-cpal: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.run() {
        Ok(()) => {
            let stats = runtime.audio_stats();
            println!(
                "soil-cpal: ok frames-written={} frames-played={} underruns={}",
                stats.frames_written, stats.frames_played, stats.underruns
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("soil-cpal: {error:#}");
            ExitCode::FAILURE
        }
    }
}
