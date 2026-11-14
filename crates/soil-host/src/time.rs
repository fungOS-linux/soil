//! `soil:time` — the host clocks.
//!
//! Two clocks, deliberately separate (see `wit/soil-time.wit`): monotonic answers
//! "how long", wall answers "when". The host owns the timezone, so this module is
//! the only place a zone offset is ever read — a guest receives resolved local
//! components and never converts from UTC itself.

use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wasmtime::component::{HasSelf, Linker};

use crate::host::WasiHost;

wasmtime::component::bindgen!({
    path: "../../wit/soil-time.wit",
    world: "host",
});

use soil::time::time as api;

/// Monotonic epoch for this host process.
///
/// `Instant` has no absolute zero, so one is fixed on first use and every guest
/// measures elapsed time from the same origin.
fn monotonic_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn monotonic_ms() -> u64 {
    u64::try_from(monotonic_epoch().elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl api::Host for WasiHost {
    fn monotonic_ms(&mut self) -> std::result::Result<u64, api::Error> {
        Ok(monotonic_ms())
    }

    fn wall_clock(&mut self) -> std::result::Result<api::DateTime, api::Error> {
        wall_clock()
    }

    fn tick(&mut self, timeout_ms: u32) -> std::result::Result<Option<u64>, api::Error> {
        // v0: a bounded wait, then the current monotonic value. There is no host
        // frame scheduler to rendezvous with yet, so this is a poll with a ceiling
        // — it never promises to wake on the next compositor frame. `0` polls
        // without blocking, as the contract requires.
        if timeout_ms > 0 {
            std::thread::sleep(Duration::from_millis(u64::from(timeout_ms.min(50))));
        }
        Ok(Some(monotonic_ms()))
    }
}

/// The current wall-clock instant, resolved in the host's timezone.
#[cfg(unix)]
fn wall_clock() -> std::result::Result<api::DateTime, api::Error> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| api::Error::NotReady)?;
    let seconds = libc::time_t::try_from(now.as_secs()).map_err(|_| api::Error::NotReady)?;

    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `tm` is a live, correctly-typed buffer and `seconds` outlives the call.
    let resolved = unsafe { libc::localtime_r(&seconds, &mut tm) };
    if resolved.is_null() {
        return Err(api::Error::NotReady);
    }

    let zone = if tm.tm_zone.is_null() {
        None
    } else {
        // SAFETY: `tm_zone` points at a NUL-terminated string owned by the C library
        // and valid for at least as long as `tm`.
        Some(
            unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }
                .to_string_lossy()
                .into_owned(),
        )
    };

    Ok(api::DateTime {
        unix_ms: u64::try_from(now.as_millis()).unwrap_or(u64::MAX),
        offset_minutes: (tm.tm_gmtoff / 60) as i32,
        year: (tm.tm_year + 1900) as u32,
        month: (tm.tm_mon + 1) as u8,
        day: tm.tm_mday as u8,
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec as u8,
        // `tm_wday` is 0 = Sunday; the contract is ISO-8601, 1 = Monday .. 7 = Sunday.
        weekday: ((tm.tm_wday + 6) % 7 + 1) as u8,
        zone,
    })
}

/// No wall clock off Unix — report `not-ready` rather than fabricate one.
#[cfg(not(unix))]
fn wall_clock() -> std::result::Result<api::DateTime, api::Error> {
    Err(api::Error::NotReady)
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_moves_forward() {
        let first = monotonic_ms();
        std::thread::sleep(Duration::from_millis(2));
        assert!(monotonic_ms() >= first, "monotonic time went backwards");
    }

    #[cfg(unix)]
    #[test]
    fn wall_clock_is_a_plausible_local_instant() {
        let now = wall_clock().expect("the host has a clock");
        // Somewhere between 2020 and 2200, so a flipped epoch or a zero fails.
        assert!(
            (1_577_836_800_000..7_258_118_400_000).contains(&now.unix_ms),
            "implausible epoch: {}",
            now.unix_ms
        );
        assert!((1..=12).contains(&now.month), "month {}", now.month);
        assert!((1..=31).contains(&now.day), "day {}", now.day);
        assert!(now.hour < 24, "hour {}", now.hour);
        assert!(now.minute < 60 && now.second < 60);
        // ISO weekday, never 0.
        assert!((1..=7).contains(&now.weekday), "weekday {}", now.weekday);
        // A real timezone is within -12h..+14h, and the offset must agree with the
        // components it accompanies; a fabricated 0 would slip past this on a UTC
        // host, which is fine — the point is catching an uninitialised `tm`.
        assert!(
            (-720..=840).contains(&now.offset_minutes),
            "offset {} minutes",
            now.offset_minutes
        );
    }

    #[test]
    fn tick_zero_polls_without_waiting() {
        let started = Instant::now();
        let tick = api::Host::tick(&mut Dummy, 0).expect("tick");
        assert!(tick.is_some());
        assert!(started.elapsed() < Duration::from_millis(40));
    }

    /// Stand-in so the tick contract can be exercised without a store.
    struct Dummy;
    impl api::Host for Dummy {
        fn monotonic_ms(&mut self) -> std::result::Result<u64, api::Error> {
            Ok(monotonic_ms())
        }
        fn wall_clock(&mut self) -> std::result::Result<api::DateTime, api::Error> {
            wall_clock()
        }
        fn tick(&mut self, timeout_ms: u32) -> std::result::Result<Option<u64>, api::Error> {
            if timeout_ms > 0 {
                std::thread::sleep(Duration::from_millis(u64::from(timeout_ms.min(50))));
            }
            Ok(Some(monotonic_ms()))
        }
    }
}
