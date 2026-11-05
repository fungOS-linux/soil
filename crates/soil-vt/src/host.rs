//! A terminal on its own thread, for hosts that must stay `Send`.
//!
//! `libghostty-vt` is `!Send` and `!Sync`: the C library keeps thread-local
//! state. A GUI app cannot hold a `Session` in its own state at all — iced moves
//! the application across threads, so one field is enough to make the whole app
//! un-sendable. Nor should it: a program that redraws a full screen of text
//! should never be able to stall the compositor.
//!
//! So the emulator gets a thread and the host gets a handle:
//!
//! ```text
//! host thread                        emulator thread
//! ───────────                        ───────────────
//! Host::write(bytes) ──────────────► Session::write
//! Host::resize(cols, rows) ────────► Session::resize
//! Host::snapshot() ◄── Arc<Mutex> ── Session::snapshot
//! ```
//!
//! Snapshots are published, not requested: the emulator thread replaces the
//! latest snapshot whenever the grid changes, and the host reads whatever is
//! current. That is what makes a 60fps redraw cheap — the render loop clones a
//! snapshot that is already built instead of asking the emulator to build one
//! per frame, and a frame that arrives mid-write simply draws the previous
//! state rather than tearing.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use crate::{Snapshot, Session};

enum Command {
    Write(Vec<u8>),
    Resize(u16, u16),
    Shutdown,
}

/// The host's end of a terminal.
pub struct Host {
    commands: Sender<Command>,
    latest: Arc<Mutex<Snapshot>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").finish_non_exhaustive()
    }
}

impl Host {
    /// Start an emulator thread and return the handle.
    pub fn spawn(cols: u16, rows: u16, scrollback: usize) -> Result<Self, crate::Error> {
        // The session is created *on* the emulator thread, not moved into it:
        // `Session` is `!Send`, so constructing it here and handing it across a
        // thread boundary does not compile — which is the whole reason this type
        // exists. An empty grid stands in until the first real snapshot lands.
        let latest = Arc::new(Mutex::new(crate::Snapshot {
            cols,
            rows,
            cells: vec![crate::Cell::default(); usize::from(cols) * usize::from(rows)],
            cursor: None,
            title: String::new(),
        }));
        let shared = Arc::clone(&latest);
        let (commands, receiver) = channel();
        let (ready, started) = channel();
        std::thread::Builder::new()
            .name("soil-vt".into())
            .spawn(move || {
                let mut session = match Session::new(cols, rows, scrollback) {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = ready.send(Err(error.to_string()));
                        return;
                    }
                };
                if let Ok(snapshot) = session.snapshot()
                    && let Ok(mut guard) = shared.lock()
                {
                    *guard = snapshot;
                }
                let _ = ready.send(Ok(()));
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Write(bytes) => session.write(&bytes),
                        Command::Resize(cols, rows) => {
                            if session.resize(cols, rows).is_err() {
                                continue;
                            }
                        }
                        Command::Shutdown => return,
                    }
                    // One snapshot per batch, not per byte: a program writing a
                    // megabyte of output costs one frame, not a thousand.
                    if let Ok(snapshot) = session.snapshot()
                        && let Ok(mut guard) = shared.lock()
                    {
                        *guard = snapshot;
                    }
                }
            })
            .map_err(|error| crate::Error(error.to_string()))?;

        // If the emulator could not start, say so here rather than handing back
        // a handle that will never produce a snapshot.
        match started.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(crate::Error(error)),
            Err(error) => return Err(crate::Error(error.to_string())),
        }

        Ok(Self { commands, latest })
    }

    /// Feed a program's output. Never blocks on the emulator.
    pub fn write(&self, bytes: &[u8]) {
        let _ = self.commands.send(Command::Write(bytes.to_vec()));
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.commands.send(Command::Resize(cols, rows));
    }

    /// The most recent grid. Cheap enough to call every frame: it clones a
    /// snapshot the emulator already built.
    pub fn snapshot(&self) -> Snapshot {
        self.latest
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poison| poison.into_inner().clone())
    }

    /// Stop the thread. Dropping the handle also ends it, because the channel
    /// closes; this is only for a host that wants to be explicit.
    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(host: &Host, needle: &str) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = host.snapshot();
            if (0..snapshot.rows).any(|y| snapshot.row_text(y).contains(needle)) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {needle:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn output_streams_in_from_another_thread() {
        let host = Host::spawn(40, 6, 100).expect("host");
        host.write(b"hello from the thread\r\n");
        let snapshot = wait_for(&host, "hello from the thread");
        assert_eq!(snapshot.row_text(0), "hello from the thread");
    }

    #[test]
    fn resizing_reflows_what_is_already_there() {
        let host = Host::spawn(40, 6, 100).expect("host");
        host.write(b"abcdef");
        wait_for(&host, "abcdef");
        host.resize(3, 6);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = host.snapshot();
            if snapshot.cols == 3 && snapshot.row_text(0) == "abc" {
                assert_eq!(snapshot.row_text(1), "def");
                break;
            }
            assert!(Instant::now() < deadline, "resize never landed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_host_is_send_and_really_is_used_across_threads() {
        // The whole point of the type: the emulator stays put, the handle moves.
        fn assert_send<T: Send + Sync>() {}
        assert_send::<Host>();

        let host = Host::spawn(30, 4, 10).expect("host");
        let handle = std::sync::Arc::new(host);
        let writer = Arc::clone(&handle);
        std::thread::spawn(move || writer.write(b"cross-thread\r\n"));
        wait_for(&handle, "cross-thread");
    }

    #[test]
    fn a_burst_of_writes_costs_one_snapshot() {
        let host = Host::spawn(200, 20, 100).expect("host");
        for _ in 0..200 {
            host.write(b"x");
        }
        let snapshot = wait_for(&host, "xxxx");
        assert!(snapshot.row_text(0).starts_with("xxxx"));
    }
}
