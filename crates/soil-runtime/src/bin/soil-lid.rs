//! Runs the existing scene guest with the permissioned HAL lid capability.
use anyhow::{Result, bail, ensure};
use soil_runtime::{
    DEFAULT_FUEL, GuestScene,
    lid::{LID_GUEST, LidGrant, Observation},
};
use std::{path::PathBuf, time::Duration};

fn main() {
    if let Err(e) = run() {
        eprintln!("soil-lid: {e:#}");
        std::process::exit(1);
    }
}
fn log(o: Observation) {
    let position = match o.position {
        None => "unknown",
        Some(fungos_device_hal::lid::LidPosition::Unqualified) => "unqualified",
        Some(fungos_device_hal::lid::LidPosition::Open) => "open",
        Some(fungos_device_hal::lid::LidPosition::Closed) => "closed",
    };
    // Debug here is human diagnostics ONLY: transport uses the HAL-owned typed codec.
    println!(
        "guest=phone-lid event=initial-or-change status={} failure={:?} position={} raw={:?} source={} event_node={} provider_monotonic_ms={} age_ms={} freshness={} policy=read-only",
        o.status(),
        o.failure,
        position,
        o.raw,
        if o.failure.is_none() {
            "hal-evdev"
        } else {
            "unknown"
        },
        o.event_node.display(),
        o.provider_monotonic_ms,
        o.age_ms,
        if o.failure.is_none() {
            "fresh"
        } else {
            "unknown"
        }
    );
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "--export-guest" {
        std::fs::write(&args[1], LID_GUEST)?;
        return Ok(());
    }
    if args.len() != 7
        || args[0] != "--guest"
        || args[2] != "--socket"
        || !matches!(args[4].as_str(), "--allow-lid" | "--deny-lid")
    {
        bail!(
            "usage: soil-lid --export-guest OUTPUT.wasm\nsoil-lid --guest MODULE.wasm --socket PATH --allow-lid|--deny-lid POLLS INTERVAL_MS\nExplicit socket required. Synthetic existing scene guest; no Canvas or display switching."
        );
    }
    let polls: usize = args[5].parse()?;
    let interval: u64 = args[6].parse()?;
    ensure!(
        (1..=600).contains(&polls) && (50..=10000).contains(&interval),
        "polls 1..600, interval 50..10000ms"
    );
    let mut bytes = Vec::new();
    use std::io::Read;
    std::fs::File::open(&args[1])?
        .take(soil_runtime::MAX_MODULE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let grant = (args[4] == "--allow-lid").then(|| LidGrant {
        socket: PathBuf::from(&args[3]),
        max_age: Duration::from_millis(1000),
    });
    let mut guest = GuestScene::with_lid(&bytes, DEFAULT_FUEL, true, grant)?;
    for o in guest.take_lid_logs() {
        log(o);
    }
    for i in 0..polls {
        // Same guest run/scene validation/GFX path, not a mock or alternate UI.
        let _image = guest.render()?;
        for o in guest.take_lid_logs() {
            log(o);
        }
        if i + 1 < polls {
            std::thread::sleep(Duration::from_millis(interval));
        }
    }
    Ok(())
}
