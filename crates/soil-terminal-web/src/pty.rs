//! A program on a pty, wired to the emulator.
//!
//! The pty is what makes a terminal a terminal: the program gets a window size,
//! `SIGWINCH` on resize, and a tty on stdin. The emulator gets the bytes and
//! publishes grids; the host writes the emulator's answers (device status
//! reports, mode queries) back to the pty, or a full-screen program would wait
//! forever for a reply.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use soil_vt::Host;

pub struct Pty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    exited: Arc<AtomicBool>,
}

impl Pty {
    pub fn spawn(command: &[String], cols: u16, rows: u16, host: Host) -> Result<Self> {
        let first = command.first().context("empty command")?;
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut builder = CommandBuilder::new(first);
        builder.args(&command[1..]);
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        builder.env("TERM_PROGRAM", "soil-terminal-web");
        let child = pair.slave.spawn_command(builder)?;
        // The child owns the slave now; keeping it open would hide its exit.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
        let exited = Arc::new(AtomicBool::new(false));

        {
            let host = host.clone();
            let exited = Arc::clone(&exited);
            thread::Builder::new()
                .name("soil-terminal-web-pty".into())
                .spawn(move || {
                    let mut buffer = [0u8; 8192];
                    loop {
                        match reader.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => host.write(&buffer[..n]),
                        }
                    }
                    exited.store(true, Ordering::Release);
                })?;
        }
        {
            let host = host.clone();
            let writer = Arc::clone(&writer);
            let exited = Arc::clone(&exited);
            thread::Builder::new()
                .name("soil-terminal-web-answers".into())
                .spawn(move || {
                    while !exited.load(Ordering::Acquire) {
                        let bytes = host.drain_responses();
                        if !bytes.is_empty()
                            && let Ok(mut writer) = writer.lock()
                        {
                            let _ = writer.write_all(&bytes);
                            let _ = writer.flush();
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                })?;
        }

        Ok(Self {
            master: Mutex::new(pair.master),
            writer,
            child: Mutex::new(child),
            exited,
        })
    }

    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_program_on_the_pty_reaches_the_emulator() {
        let host = Host::spawn(40, 6, 100).unwrap();
        let _pty = Pty::spawn(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf 'hello from the pty\\r\\n'; sleep 1".to_string(),
            ],
            40,
            6,
            host.clone(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = host.snapshot();
            if (0..snapshot.rows).any(|y| snapshot.row_text(y).contains("hello from the pty")) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for pty output; screen: {:?}",
                (0..snapshot.rows)
                    .map(|y| snapshot.row_text(y))
                    .collect::<Vec<_>>()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
