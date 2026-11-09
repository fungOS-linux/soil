//! Execution deadlines for Mush commands.
//!
//! The command engine is a Wasm Spore, so a command that outlives its caller's
//! patience would otherwise block that caller forever. For an agent harness
//! that is the worst failure available: no output, no exit status, no way to
//! recover except killing the tool call.
//!
//! Wasmtime epoch interruption is the only mechanism here that actually stops
//! the guest, so every non-interactive command runs with a timer armed. An
//! interactive session opts out, because there the human is the clock.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use wasmtime::Engine;

/// Budget for a command that nobody is watching run.
///
/// This is deliberately loose: a cold `mb` invocation is already seconds of
/// host time, and a trap at the wrong moment is worse than a slow answer.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a waiting timer checks whether the command already finished.
const POLL: Duration = Duration::from_millis(25);

/// A one-shot deadline for a single command.
///
/// Firing is recorded, so the caller never has to infer a timeout from elapsed
/// time: `fired()` is true only when this timer actually interrupted the guest.
pub struct Timer {
    fired: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl Timer {
    /// Arm `engine` to trap the guest after `budget`.
    ///
    /// The caller must set the store deadline to one epoch tick so the single
    /// bump this timer performs is what interrupts the command.
    pub fn arm(engine: Engine, budget: Duration) -> Self {
        let fired = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let fired_by_timer = Arc::clone(&fired);
        let finished = Arc::clone(&done);
        thread::spawn(move || {
            let deadline = std::time::Instant::now() + budget;
            while std::time::Instant::now() < deadline {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                thread::sleep(left.min(POLL));
                if finished.load(Ordering::SeqCst) {
                    return;
                }
            }
            // Claim the right to interrupt. If the command finished first the
            // claim fails and no epoch bump is delivered to a later command.
            if finished
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                fired_by_timer.store(true, Ordering::SeqCst);
                engine.increment_epoch();
            }
        });
        Self { fired, done }
    }

    /// Whether this timer interrupted the command.
    pub fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    /// Mark the command as finished so the timer stops waiting to fire.
    pub fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
    }
}

/// Parse a `--timeout` value in seconds. `0` disables the deadline.
pub fn parse_budget(value: &str) -> Result<Option<Duration>> {
    let seconds: f64 = value
        .trim()
        .parse()
        .with_context(|| format!("parse timeout {value:?} as seconds"))?;
    if !seconds.is_finite() || seconds < 0.0 {
        bail!("timeout must be a non-negative number of seconds, got {value:?}");
    }
    if seconds == 0.0 {
        return Ok(None);
    }
    Ok(Some(Duration::from_secs_f64(seconds)))
}

/// The line a caller sees instead of silence when a command runs out of time.
pub fn timeout_hint(budget: Duration) -> String {
    format!(
        "mush: command exceeded {}s and was interrupted\n\
         mush: hint: raise it with --timeout SECONDS, or split the work into steps that finish\n",
        budget.as_secs_f64()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_disables_the_deadline() {
        assert_eq!(parse_budget("0").unwrap(), None);
        assert_eq!(parse_budget("30").unwrap(), Some(Duration::from_secs(30)));
        assert_eq!(
            parse_budget("0.5").unwrap(),
            Some(Duration::from_millis(500))
        );
    }

    #[test]
    fn rejects_negative_and_garbage_budgets() {
        assert!(parse_budget("-1").is_err());
        assert!(parse_budget("soon").is_err());
        assert!(parse_budget("inf").is_err());
    }
}
