//! Small hardware probe for the host capture adapters.
//!
//! ```text
//! cargo run -p soil-capture --bin soil-capture-probe -- mic --seconds 3
//! cargo run -p soil-capture --bin soil-capture-probe -- camera --device 0 --out /tmp/frame.ppm
//! ```

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use soil_capture::camera::{CameraSource, FfmpegCameraSource, PixelFormat};
use soil_capture::mic::{CpalMicSource, MicSource};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-capture-probe: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("mic") => mic(args),
        Some("camera") => camera(args),
        _ => {
            eprintln!("usage: soil-capture-probe mic [--seconds N]");
            eprintln!(
                "       soil-capture-probe camera [--device ID] [--seconds N] [--out FILE.ppm]"
            );
            Ok(())
        }
    }
}

fn mic(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut seconds = 3_u64;
    while let Some(arg) = args.next() {
        if arg == "--seconds" {
            seconds = args.next().ok_or("--seconds needs a value")?.parse()?;
        }
    }
    let source = CpalMicSource::new();
    let mut session = source.open("soil-capture-probe microphone test")?;
    let format = session.format();
    println!(
        "microphone: {} Hz, {} channel(s), {seconds}s",
        format.sample_rate, format.channels
    );

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut frames = 0_u64;
    let mut sum_squares = 0.0_f64;
    let mut peak = 0.0_f32;
    let mut chunks = 0_u64;
    while Instant::now() < deadline {
        let chunk = session.read(1600, Duration::from_millis(250))?;
        let Some(chunk) = chunk else { continue };
        frames += u64::from(chunk.frames);
        chunks += 1;
        for sample in chunk.samples {
            sum_squares += f64::from(sample) * f64::from(sample);
            peak = peak.max(sample.abs());
        }
    }
    let _ = session.close();
    let samples = frames.saturating_mul(u64::from(format.channels));
    let rms = if samples == 0 {
        0.0
    } else {
        (sum_squares / samples as f64).sqrt()
    };
    println!("microphone: chunks={chunks} frames={frames} rms={rms:.5} peak={peak:.5}");
    if frames == 0 {
        return Err("microphone delivered no frames".into());
    }
    Ok(())
}

fn camera(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut device = "0".to_string();
    let mut seconds = 2_u64;
    let mut out = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--device" => device = args.next().ok_or("--device needs a value")?,
            "--seconds" => seconds = args.next().ok_or("--seconds needs a value")?.parse()?,
            "--out" => out = Some(args.next().ok_or("--out needs a value")?),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let source = FfmpegCameraSource::new(device);
    let mut session = source.open("soil-capture-probe camera test")?;
    let format = session.format();
    println!(
        "camera: {}x{} {:?}, {}s",
        format.width, format.height, format.pixel_format, seconds
    );
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut first = None;
    let mut count = 0_u64;
    while Instant::now() < deadline {
        if let Some(frame) = session.next_frame(Duration::from_millis(500))? {
            count += 1;
            if first.is_none() {
                first = Some(frame);
            }
        }
    }
    let _ = session.close();
    let Some(frame) = first else {
        return Err("camera delivered no frames".into());
    };
    if let Some(path) = out {
        write_ppm(&path, &frame)?;
        println!("camera: wrote {path} ({} frames read)", count);
    } else {
        println!("camera: frames={count} first_sequence={}", frame.sequence);
    }
    Ok(())
}

fn write_ppm(
    path: &str,
    frame: &soil_capture::camera::CameraFrame,
) -> Result<(), Box<dyn std::error::Error>> {
    if frame.format.pixel_format != PixelFormat::Rgba8 {
        return Err("only rgba8 frames can be written as PPM".into());
    }
    let mut file = BufWriter::new(File::create(path)?);
    write!(
        file,
        "P6\n{} {}\n255\n",
        frame.format.width, frame.format.height
    )?;
    for rgba in frame.data.chunks_exact(4) {
        file.write_all(&rgba[..3])?;
    }
    file.flush()?;
    Ok(())
}
