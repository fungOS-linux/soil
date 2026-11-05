//! Local headless owner render capture; no device or presentation access.
use s22_canvas_surface::{CanvasSurface, Key};
use std::io::Write;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: capture NEW_OUTPUT.ppm [WIDTH HEIGHT]")?;
    let width = args
        .next()
        .map(|v| v.parse::<u32>())
        .transpose()?
        .unwrap_or(480);
    let height = args
        .next()
        .map(|v| v.parse::<u32>())
        .transpose()?
        .unwrap_or(640);
    let jarvis = match args.next().as_deref() {
        None => false,
        Some("simple-jarvis") => true,
        Some(_) => return Err("expected optional simple-jarvis".into()),
    };
    let preview_state = args.next();
    if args.next().is_some() {
        return Err("unexpected capture argument".into());
    }
    let mut surface = if jarvis {
        CanvasSurface::simple_jarvis(width, height)?
    } else {
        CanvasSurface::new(width, height)?
    };
    if let Some(state) = preview_state {
        use s22_canvas_surface::{Availability, BatteryView};
        let availability = match state.as_str() {
            "fixture-estimate" => Availability::Fresh,
            "fixture-voltage" => Availability::Fresh,
            "fixture-stale" => Availability::Stale,
            "fixture-denied" => Availability::Denied,
            "fixture-unknown" => Availability::Unknown,
            _ => return Err("expected fixture-voltage/stale/denied/unknown".into()),
        };
        if state == "fixture-estimate" {
            surface.update_model_estimate(s22_canvas_surface::ModelEstimateView {
                percent: Some(57),
                availability,
            })?;
        }
        surface.update_status(BatteryView {
            percent: None,
            voltage_uv: Some(3_879_000),
            availability,
        })?;
    }
    surface.present()?;
    surface.key(Key::Down)?;
    let frame = surface.present()?;
    let mut out = std::fs::File::create_new(path)?;
    write!(out, "P6\n{} {}\n255\n", frame.width, frame.height)?;
    for pixel in frame.rgba.as_chunks::<4>().0 {
        out.write_all(&pixel[..3])?;
    }
    Ok(())
}
