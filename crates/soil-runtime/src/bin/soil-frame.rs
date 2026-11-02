//! Host output/input adapter only: no DRM or service ownership.
use anyhow::{Context, Result, bail};
use soil_runtime::{DEFAULT_FUEL, GuestScene, Key, PHONE_SHELL};
use std::{
    fs,
    io::{self, BufRead, Read},
    path::Path,
};

fn write_frame(guest: &mut GuestScene, path: &Path, format: &str) -> Result<()> {
    let image = guest.render()?;
    let bytes = match format {
        "rgba" => image.pixels,
        "bgr24" => image.to_bgr24()?,
        _ => bail!("format must be rgba or bgr24"),
    };
    // Same-directory rename: readers see either the old or the complete new frame.
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn main() {
    if let Err(error) = execute() {
        eprintln!("soil-frame: {error:#}");
        std::process::exit(1);
    }
}

fn execute() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(args.len() == 3
        || (args.len() == 4 && args[3] == "--stdin")
        || (args.len() == 7 && args[3] == "--evdev"))
    {
        bail!(
            "usage: soil-frame demo|MODULE.wasm OUTPUT rgba|bgr24 [--stdin | --evdev DEVICE MAX_EVENTS TIMEOUT_MS]\nDemo is a synthetic fixture, NOT Canvas.\nstdin lines: up down left right ok back quit"
        );
    }
    if !matches!(args[2].as_str(), "rgba" | "bgr24") {
        bail!("unknown output format");
    }
    let bytes = if args[0] == "demo" {
        PHONE_SHELL.to_vec()
    } else {
        let mut bytes = Vec::new();
        fs::File::open(&args[0])?
            .take(soil_runtime::MAX_MODULE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        bytes
    };
    let mut guest = GuestScene::new(&bytes, DEFAULT_FUEL)?;
    let path = Path::new(&args[1]);
    write_frame(&mut guest, path, &args[2])?;
    if args.len() == 4 {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        loop {
            // Bound each input line; never accumulate an unbounded stdin string.
            let mut line = String::new();
            let n = input.by_ref().take(65).read_line(&mut line)?;
            if n == 0 {
                break;
            }
            if n > 64 {
                bail!("input line too long");
            }
            let key = match line.trim() {
                "up" => Key::Up,
                "down" => Key::Down,
                "left" => Key::Left,
                "right" => Key::Right,
                "ok" => Key::Select,
                "back" => Key::Back,
                "quit" => break,
                _ => bail!("unknown normalized key"),
            };
            guest.key(key).context("apply normalized key")?;
            write_frame(&mut guest, path, &args[2])?;
        }
    }
    if args.len() == 7 {
        #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
        {
            let count = soil_runtime::evdev::read_device(
                Path::new(&args[4]),
                args[5].parse()?,
                std::time::Duration::from_millis(args[6].parse()?),
                |key| {
                    guest.key(key)?;
                    write_frame(&mut guest, path, &args[2])
                },
            )?;
            eprintln!("soil-frame: processed {count} evdev records");
        }
        #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
        bail!("evdev requires 64-bit Linux");
    }
    Ok(())
}
