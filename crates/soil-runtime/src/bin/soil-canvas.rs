//! Actual owner Canvas surface via permissioned Wasm. No device/DRM ownership.
use anyhow::{Result, bail, ensure};
use soil_runtime::{
    DEFAULT_FUEL, GuestScene,
    battery::{BATTERY_CANVAS_GUEST, BatteryGrant},
    canvas::{ANIMATED_BATTERY_GUEST, CANVAS_GUEST, OwnerSurface, SurfaceSize},
};
use std::{io::Read, path::Path, time::Duration};

fn main() {
    if let Err(e) = run() {
        eprintln!("soil-canvas: {e:#}");
        std::process::exit(1);
    }
}
fn publish(guest: &mut GuestScene, path: &Path, clock: Option<std::time::Instant>) -> Result<()> {
    let t0 = std::time::Instant::now();
    if let Some(start) = clock {
        guest.tick(u64::try_from(start.elapsed().as_millis())?)?;
    }
    let t1 = std::time::Instant::now();
    let frame = guest.render_canvas()?;
    let t2 = std::time::Instant::now();
    let bgr = frame.to_bgr24()?;
    let t3 = std::time::Instant::now();
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, bgr)?;
    std::fs::rename(&temporary, path)?;
    let t4 = std::time::Instant::now();
    if std::env::var_os("SOIL_CANVAS_TIMING").is_some() {
        eprintln!(
            "timing tick={:?} render={:?} bgr={:?} write={:?} total={:?}",
            t1 - t0,
            t2 - t1,
            t3 - t2,
            t4 - t3,
            t4 - t0
        );
    }
    Ok(())
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2
        && matches!(
            args[0].as_str(),
            "--export-guest" | "--export-battery-guest" | "--export-animated-battery-guest"
        )
    {
        std::fs::write(
            &args[1],
            if args[0] == "--export-animated-battery-guest" {
                ANIMATED_BATTERY_GUEST
            } else if args[0] == "--export-battery-guest" {
                BATTERY_CANVAS_GUEST
            } else {
                CANVAS_GUEST
            },
        )?;
        return Ok(());
    }
    // Optional host-only surface selector is last, preserving existing CLI.
    let mut args = args;
    let animation = if let Some(index) = args.iter().position(|a| a == "--animate-reactor") {
        args.remove(index);
        true
    } else {
        false
    };
    // Battery authority is host-selected, explicit, and independent of keypad.
    let mut battery_socket = None;
    let mut battery_permission = None;
    let mut keypad_socket = None;
    let mut keypad_permission = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--battery-socket" => {
                ensure!(battery_socket.is_none(), "duplicate battery socket");
                let socket = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("--battery-socket requires PATH"))?
                    .clone();
                ensure!(!socket.starts_with("--"), "--battery-socket requires PATH");
                battery_socket = Some(std::path::PathBuf::from(socket));
                args.drain(index..index + 2);
            }
            "--allow-battery" | "--deny-battery" => {
                ensure!(battery_permission.is_none(), "duplicate battery permission");
                battery_permission = Some(args[index] == "--allow-battery");
                args.remove(index);
            }
            "--keypad-socket" => {
                ensure!(keypad_socket.is_none(), "duplicate keypad socket");
                let socket = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("--keypad-socket requires PATH"))?
                    .clone();
                ensure!(!socket.starts_with("--"), "--keypad-socket requires PATH");
                keypad_socket = Some(std::path::PathBuf::from(socket));
                args.drain(index..index + 2);
            }
            "--allow-keypad" | "--deny-keypad" => {
                ensure!(keypad_permission.is_none(), "duplicate keypad permission");
                keypad_permission = Some(args[index] == "--allow-keypad");
                args.remove(index);
            }
            _ => index += 1,
        }
    }
    ensure!(
        battery_permission != Some(true) || battery_socket.is_some(),
        "--allow-battery requires explicit --battery-socket PATH"
    );
    ensure!(
        battery_socket.is_none() || battery_permission.is_some(),
        "battery socket requires --allow-battery or --deny-battery"
    );
    ensure!(
        keypad_permission != Some(true) || keypad_socket.is_some(),
        "--allow-keypad requires explicit --keypad-socket PATH"
    );
    ensure!(
        keypad_socket.is_none() || keypad_permission.is_some(),
        "keypad socket requires --allow-keypad or --deny-keypad"
    );
    let simple_jarvis = if let Some(index) = args.iter().position(|arg| arg == "--ui") {
        ensure!(
            args.get(index + 1).map(String::as_str) == Some("simple-jarvis"),
            "--ui requires simple-jarvis"
        );
        args.drain(index..index + 2);
        true
    } else {
        false
    };
    ensure!(
        simple_jarvis || battery_permission.is_none(),
        "battery status requires --ui simple-jarvis"
    );
    ensure!(
        !animation || (simple_jarvis && battery_permission.is_some()),
        "--animate-reactor requires simple-jarvis and explicit battery permission"
    );
    let surface = if args.len() >= 2 && args[args.len() - 2] == "--surface" {
        let size = match args.last().map(String::as_str) {
            Some("inner") => SurfaceSize::Inner,
            Some("outer") => SurfaceSize::Outer,
            _ => bail!("surface must be inner or outer"),
        };
        args.truncate(args.len() - 2);
        size
    } else {
        SurfaceSize::Inner
    };
    // Deployment policy: inner is the sole navigation input owner. Outer stays
    // passive; never create a second independently advancing input consumer.
    ensure!(
        surface != SurfaceSize::Outer || keypad_permission != Some(true),
        "outer surface is passive; grant keypad only to inner producer"
    );
    if args.len() != 8
        || args[0] != "--guest"
        || args[2] != "--output"
        || args[4] != "--frames"
        || args[6] != "--interval-ms"
    {
        bail!(
            "usage: soil-canvas --export-guest FILE.wasm\nsoil-canvas --guest FILE.wasm --output FILE.bgr --frames N --interval-ms MS [--keypad-socket PATH --allow-keypad|--deny-keypad] [--surface inner|outer] [--ui simple-jarvis [--battery-socket PATH --allow-battery|--deny-battery]]\nActual Canvas owner, explicit DEMO DATA. Atomic BGR24; inner480x640 or outer128x128; no DRM ownership."
        );
    }
    let frames: usize = args[5].parse()?;
    let interval: u64 = args[7].parse()?;
    ensure!(
        (1..=36000).contains(&frames) && (50..=5000).contains(&interval),
        "frames1..36000 interval50..5000ms"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(&args[1])?
        .take(soil_runtime::MAX_MODULE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let owner = if simple_jarvis {
        OwnerSurface::simple_jarvis(surface)?
    } else {
        OwnerSurface::with_size(surface)?
    };
    let mut guest = if battery_permission.is_some() {
        let grant = if battery_permission == Some(true) {
            Some(BatteryGrant::new(
                battery_socket.expect("validated battery socket"),
                Duration::from_millis(1000),
            ))
        } else {
            None
        };
        if animation {
            GuestScene::with_animated_battery_canvas(
                &bytes,
                DEFAULT_FUEL,
                Some(Box::new(owner)),
                grant,
            )?
        } else {
            GuestScene::with_battery_canvas(&bytes, DEFAULT_FUEL, Some(Box::new(owner)), grant)?
        }
    } else {
        GuestScene::with_canvas(&bytes, DEFAULT_FUEL, Some(Box::new(owner)))?
    };
    let (width, height) = surface.dimensions();
    let path = Path::new(&args[3]);
    let mut keypad = keypad_permission.map(|allowed| {
        soil_runtime::keypad::KeypadGrant::new(if allowed { keypad_socket } else { None })
    });
    let mut last_failure = None;
    let clock = animation.then(std::time::Instant::now);
    for i in 0..frames {
        let frame_start = std::time::Instant::now();
        if let Some(keypad) = &mut keypad {
            match keypad.poll() {
                Ok(keys) => {
                    if last_failure.take().is_some() {
                        eprintln!("keypad=provider transport=recovered held=cleared");
                    }
                    // Render after each event: Canvas owner consumes queued UI events
                    // during present; do not coalesce Select/Back semantics.
                    for key in keys {
                        guest.key(key)?;
                        publish(&mut guest, path, clock)?;
                        eprintln!("keypad=provider normalized={key:?} guest=input canvas=owner");
                    }
                }
                Err(e) => {
                    if last_failure != Some(e) {
                        eprintln!("keypad=provider failure={e:?} held=cleared");
                    }
                    last_failure = Some(e);
                }
            }
        }
        publish(&mut guest, path, clock)?;
        if i == 0 {
            println!(
                "canvas={} provenance=owner-demo frame={} width={width} height={height} format=bgr24 startup=ready",
                if simple_jarvis {
                    "simple-jarvis"
                } else {
                    "work-inbox"
                },
                path.display()
            );
        }
        if i + 1 < frames {
            // interval is a frame period, not a sleep-after-work budget. Keep
            // the cadence when rendering is cheaper than the period; run flat
            // out when it is not.
            let period = Duration::from_millis(interval);
            let elapsed = frame_start.elapsed();
            if elapsed < period {
                std::thread::sleep(period - elapsed);
            }
        }
    }
    println!(
        "canvas={} exit=complete last-frame=retained",
        if simple_jarvis {
            "simple-jarvis"
        } else {
            "work-inbox"
        }
    );
    Ok(())
}
