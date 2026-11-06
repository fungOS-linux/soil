//! Render one frame of a resident live visualization without opening a window.
//!
//! Useful for screenshots and CI. The window host uses the same `LiveViz`.

use soil_scene_gpu::live::{LiveSpec, LiveViz};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut spec_path = None;
    let mut rgba_path = None;
    let mut time = 0.0f32;
    let mut width = 1200u32;
    let mut height = 840u32;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--spec" => spec_path = args.next(),
            "--rgba" => rgba_path = args.next(),
            "--time" => {
                time = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--time needs seconds"))?
                    .parse()?
            }
            "--width" => {
                width = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--width needs a number"))?
                    .parse()?
            }
            "--height" => {
                height = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--height needs a number"))?
                    .parse()?
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let spec_path = spec_path.ok_or_else(|| anyhow::anyhow!("missing --spec"))?;
    let rgba_path = rgba_path.ok_or_else(|| anyhow::anyhow!("missing --rgba"))?;
    let bytes = std::fs::read(&spec_path)?;
    let spec: LiveSpec = serde_json::from_slice(&bytes)?;
    width = width.min(4096);
    height = height.min(4096);
    let mut live = LiveViz::new(spec);
    if time > 0.0 {
        live.advance(time);
    }
    let frame = live.render(width, height)?;
    std::fs::write(&rgba_path, &frame.rgba)?;
    println!(
        "soil-live-frame: wrote {rgba_path} {}x{}",
        frame.width, frame.height
    );
    Ok(())
}
