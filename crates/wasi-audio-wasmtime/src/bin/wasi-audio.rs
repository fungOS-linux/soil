//! Run one component that imports `wasi:audio` and exports `run`.
//!
//! Usage:
//!   wasi-audio COMPONENT.wasm
//!   wasi-audio COMPONENT.wasm --isochrone-destination 192.168.20.13:50040
//!   wasi-audio COMPONENT.wasm --isochrone-config /etc/soil/audio.json

use std::{env, fs, net::SocketAddr, process::ExitCode};

use wasi_audio_wasmtime::{AudioRuntime, IsochroneConfig, WasiAudioHost};

fn main() -> ExitCode {
    match run() {
        Ok(0) => {
            println!("audio: ok");
            ExitCode::SUCCESS
        }
        Ok(status) => {
            eprintln!("audio: guest status {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("audio: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u32, String> {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        return Err(
            "usage: wasi-audio COMPONENT.wasm [--isochrone-config FILE] [--isochrone-destination ADDR] [--isochrone-bind ADDR] [--isochrone-ssrc N]"
                .into(),
        );
    };
    let mut config = None;
    let mut destination = None;
    let mut bind = None;
    let mut ssrc = 0x536f_696c;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--isochrone-config" => {
                let path = args.next().ok_or("--isochrone-config requires FILE")?;
                let bytes = fs::read(&path).map_err(|error| format!("read {path}: {error}"))?;
                config = Some(
                    serde_json::from_slice::<IsochroneConfig>(&bytes)
                        .map_err(|error| format!("parse {path}: {error}"))?,
                );
            }
            "--isochrone-destination" => {
                destination = Some(
                    args.next()
                        .ok_or("--isochrone-destination requires ADDR")?
                        .parse::<SocketAddr>()
                        .map_err(|error| format!("destination: {error}"))?,
                );
            }
            "--isochrone-bind" => {
                bind = Some(
                    args.next()
                        .ok_or("--isochrone-bind requires ADDR")?
                        .parse::<SocketAddr>()
                        .map_err(|error| format!("bind: {error}"))?,
                );
            }
            "--isochrone-ssrc" => {
                ssrc = args
                    .next()
                    .ok_or("--isochrone-ssrc requires N")?
                    .parse()
                    .map_err(|error| format!("ssrc: {error}"))?;
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if let Some(destination) = destination {
        config = Some(IsochroneConfig {
            bind: bind.unwrap_or_else(|| "0.0.0.0:0".parse().expect("valid bind")),
            destination,
            ssrc,
        });
    }
    let bytes = fs::read(&path).map_err(|error| format!("read {path}: {error}"))?;
    let runtime = AudioRuntime::new().map_err(|error| format!("runtime: {error:#}"))?;
    let host = match config {
        Some(config) => WasiAudioHost::with_isochrone(config),
        None => WasiAudioHost::new(),
    };
    runtime
        .run_component_with_host(&bytes, host)
        .map_err(|error| format!("{error:#}"))
}
