//! Host-only Linux input_event decoder for 64-bit Linux (including ARM64).
//! No device is opened by the guest. Unknown keys are never forwarded.
use crate::Key;

pub const EVENT_BYTES: usize = 24;
pub const MAX_EVENTS: usize = 4096;

#[derive(Default)]
pub struct Keypad {
    held: [bool; 7],
    dropped: bool,
}

impl Keypad {
    /// Decode a native-endian 64-bit Linux input_event. Timestamps have no
    /// authority. Releases only clear state; repeats navigate held arrows only.
    /// SYN_DROPPED discards until SYN_REPORT, then requires a fresh press.
    pub fn event(&mut self, bytes: &[u8; EVENT_BYTES]) -> Option<Key> {
        let kind = u16::from_ne_bytes(bytes[16..18].try_into().unwrap());
        let code = u16::from_ne_bytes(bytes[18..20].try_into().unwrap());
        let value = i32::from_ne_bytes(bytes[20..24].try_into().unwrap());
        if kind == 0 && code == 3 {
            self.held.fill(false);
            self.dropped = true;
            return None;
        }
        if self.dropped {
            if kind == 0 && code == 0 {
                self.dropped = false;
            }
            return None;
        }
        if kind != 1 {
            return None;
        }
        let key = match code {
            103 => Key::Up,
            108 => Key::Down,
            105 => Key::Left,
            106 => Key::Right,
            28 | 352 => Key::Select,
            1 | 158 => Key::Back,
            172 => Key::Home,
            _ => return None,
        };
        let index = key as usize - 1;
        match value {
            0 => {
                self.held[index] = false;
                None
            }
            1 if !self.held[index] => {
                self.held[index] = true;
                Some(key)
            }
            2 if self.held[index] && index < 4 => Some(key),
            _ => None,
        }
    }
}

/// Open a caller-selected device read-only, nonblocking, without EVIOCGRAB.
/// Work and elapsed time are bounded. Callbacks are normalized host events.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn read_device(
    path: &std::path::Path,
    max_events: usize,
    timeout: std::time::Duration,
    mut action: impl FnMut(Key) -> anyhow::Result<()>,
) -> anyhow::Result<usize> {
    use std::{
        io::Read,
        os::unix::fs::OpenOptionsExt,
        time::{Duration, Instant},
    };
    anyhow::ensure!(
        (1..=MAX_EVENTS).contains(&max_events),
        "event budget must be 1..=4096"
    );
    anyhow::ensure!(
        !timeout.is_zero() && timeout <= Duration::from_secs(60),
        "timeout must be 1..=60000ms"
    );
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let mut keypad = Keypad::default();
    let start = Instant::now();
    let mut record = [0; EVENT_BYTES];
    let mut used = 0;
    let mut events = 0;
    while events < max_events && start.elapsed() < timeout {
        match file.read(&mut record[used..]) {
            Ok(0) => anyhow::bail!("evdev disconnected or end of stream"),
            Ok(n) => {
                used += n;
                if used == EVENT_BYTES {
                    events += 1;
                    if let Some(key) = keypad.event(&record) {
                        action(key)?;
                    }
                    used = 0;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::ensure!(used == 0, "truncated evdev event");
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(kind: u16, code: u16, value: i32) -> [u8; EVENT_BYTES] {
        let mut b = [0; EVENT_BYTES];
        b[16..18].copy_from_slice(&kind.to_ne_bytes());
        b[18..20].copy_from_slice(&code.to_ne_bytes());
        b[20..].copy_from_slice(&value.to_ne_bytes());
        b
    }
    #[test]
    fn press_repeat_release_and_unknown() {
        let mut k = Keypad::default();
        assert!(k.event(&event(1, 108, 2)).is_none());
        assert_eq!(k.event(&event(1, 108, 1)), Some(Key::Down));
        assert!(k.event(&event(1, 108, 1)).is_none());
        assert_eq!(k.event(&event(1, 108, 2)), Some(Key::Down));
        assert!(k.event(&event(1, 108, 0)).is_none());
        assert!(k.event(&event(1, 108, 2)).is_none());
        for (kind, code, value) in [(1, 999, 1), (2, 103, 1), (1, 103, -1), (1, 103, 3)] {
            assert!(k.event(&event(kind, code, value)).is_none());
        }
        for code in [28, 352, 1, 158] {
            assert!(k.event(&event(1, code, 1)).is_some());
            assert!(k.event(&event(1, code, 2)).is_none());
            assert!(k.event(&event(1, code, 0)).is_none());
        }
    }
    #[test]
    fn normalized_keys_reuse_viewer_owner_contract() {
        use viewer_spore::{Target, TargetKind, Viewer};
        let mut viewer =
            Viewer::new([Target::new("proof", TargetKind::Retained, "Proof", 4).unwrap()]).unwrap();
        viewer.open("proof").unwrap();
        let mut keypad = Keypad::default();
        for (code, position) in [(106, 1), (106, 2), (105, 1), (158, 0)] {
            let key = keypad.event(&event(1, code, 1)).unwrap();
            assert_eq!(
                viewer.remote(key.viewer_key()).unwrap().unwrap().position,
                position
            );
            assert!(keypad.event(&event(1, code, 0)).is_none());
        }
        assert!(viewer.remote(Key::Up.viewer_key()).unwrap().is_none());
        assert!(viewer.remote(Key::Down.viewer_key()).unwrap().is_none());
        assert_eq!(
            viewer
                .remote(Key::Select.viewer_key())
                .unwrap()
                .unwrap()
                .position,
            0
        );
    }

    #[test]
    fn lost_sync_requires_fresh_press() {
        let mut k = Keypad::default();
        assert_eq!(k.event(&event(1, 103, 1)), Some(Key::Up));
        assert!(k.event(&event(0, 3, 0)).is_none());
        assert!(k.event(&event(1, 103, 1)).is_none());
        assert!(k.event(&event(0, 0, 0)).is_none());
        assert!(k.event(&event(1, 103, 2)).is_none());
        assert_eq!(k.event(&event(1, 103, 1)), Some(Key::Up));
    }
    #[test]
    fn evdev_drives_only_normalized_guest_input() {
        let mut guest = crate::GuestScene::new(crate::PHONE_SHELL, crate::DEFAULT_FUEL).unwrap();
        let before = guest.render().unwrap();
        let mut k = Keypad::default();
        for value in [1, 0] {
            if let Some(key) = k.event(&event(1, 108, value)) {
                guest.key(key).unwrap();
            }
        }
        assert_ne!(before, guest.render().unwrap());
        for value in [1, 0] {
            if let Some(key) = k.event(&event(1, 103, value)) {
                guest.key(key).unwrap();
            }
        }
        assert_eq!(before, guest.render().unwrap());
    }
}
