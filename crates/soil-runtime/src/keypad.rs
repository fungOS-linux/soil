//! HAL-owned typed keypad transport, host-side normalization only.
//! No evdev reader or device owner here. Keymap source: reversal
//! cat-s22/mainline/kernel/dts/cat-s22-keypad.dtsi (Back row4,col0=0x009e).
use crate::Key;
use fungos_device_hal::{
    keypad::{KeyAction, KeypadBatch, KeypadCursor, KeypadFailure},
    keypad_transport::KeypadClient,
};
use std::path::PathBuf;

#[derive(Default)]
pub struct Normalize {
    held: [bool; 7],
}
impl Normalize {
    pub fn clear(&mut self) {
        self.held.fill(false);
    }
    pub fn batch(&mut self, batch: &KeypadBatch) -> Vec<Key> {
        if batch.resync.is_some() {
            self.clear();
            return Vec::new();
        }
        batch
            .events
            .iter()
            .filter_map(|event| {
                let key = match event.code {
                    103 => Key::Up,
                    108 => Key::Down,
                    105 => Key::Left,
                    106 => Key::Right,
                    28 => Key::Select,
                    158 => Key::Back,
                    172 => Key::Home,
                    _ => return None,
                };
                let i = key as usize - 1;
                match event.action {
                    KeyAction::Release => {
                        self.held[i] = false;
                        None
                    }
                    KeyAction::Press if !self.held[i] => {
                        self.held[i] = true;
                        Some(key)
                    }
                    KeyAction::Repeat if self.held[i] && i < 4 => Some(key),
                    _ => None,
                }
            })
            .collect()
    }
}
/// Explicit per-guest permission; denied poll does not contact the provider.
pub struct KeypadGrant {
    client: Option<KeypadClient>,
    cursor: Option<KeypadCursor>,
    normalize: Normalize,
}
impl KeypadGrant {
    pub fn new(socket: Option<PathBuf>) -> Self {
        Self {
            client: socket.map(KeypadClient::new),
            cursor: None,
            normalize: Normalize::default(),
        }
    }
    #[cfg(test)]
    fn held_keys(&self) -> bool {
        self.normalize.held.iter().any(|held| *held)
    }
    pub fn poll(&mut self) -> Result<Vec<Key>, KeypadFailure> {
        let Some(client) = self.client.as_mut() else {
            return Err(KeypadFailure::Denied);
        };
        match client.poll(self.cursor) {
            Ok(batch) => {
                self.cursor = Some(batch.cursor);
                Ok(self.normalize.batch(&batch))
            }
            Err(e) => {
                self.normalize.clear();
                Err(e)
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use fungos_device_hal::keypad::{KeypadEvent, KeypadResync};
    fn batch(events: &[(u16, KeyAction)]) -> KeypadBatch {
        KeypadBatch {
            cursor: KeypadCursor {
                provider_instance: 1,
                sequence: events.len() as u64,
            },
            resync: None,
            events: events
                .iter()
                .enumerate()
                .map(|(i, (code, action))| KeypadEvent {
                    sequence: i as u64 + 1,
                    code: *code,
                    action: *action,
                    observed_monotonic_ms: 0,
                })
                .collect(),
        }
    }
    #[test]
    fn typed_press_repeat_release_and_resync() {
        let mut n = Normalize::default();
        use KeyAction::*;
        assert_eq!(
            n.batch(&batch(&[
                (108, Press),
                (108, Press),
                (108, Repeat),
                (108, Release),
                (108, Repeat),
                (28, Press),
                (28, Repeat),
                (158, Press),
                (172, Press),
                (172, Repeat),
                (172, Release),
                (172, Repeat),
                (580, Press),
                (115, Press),
                (114, Press),
                (999, Press)
            ])),
            vec![Key::Down, Key::Down, Key::Select, Key::Back, Key::Home]
        );
        for resync in [
            KeypadResync::NewClient,
            KeypadResync::Overflow,
            KeypadResync::Restart,
            KeypadResync::Dropped,
            KeypadResync::Stale,
            KeypadResync::Reconnect,
        ] {
            n.batch(&batch(&[(103, Press)]));
            let mut b = batch(&[(103, Repeat)]);
            b.resync = Some(resync);
            assert!(n.batch(&b).is_empty());
            assert!(n.batch(&batch(&[(103, Repeat)])).is_empty());
            assert_eq!(n.batch(&batch(&[(103, Press)])), vec![Key::Up]);
        }
    }
    #[test]
    fn stale_disconnect_reconnect_clear_held_and_require_new_press() {
        use fungos_device_hal::{
            keypad::KeypadRing,
            keypad_transport::{decode_request, encode_response},
        };
        use std::{
            io::{Read, Write},
            os::unix::net::UnixListener,
        };
        let path = std::env::temp_dir().join(format!("soil-key-held-{}", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let thread = std::thread::spawn(move || {
            let mut ring = KeypadRing::new(77);
            for i in 0..10 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                stream.read_to_string(&mut request).unwrap();
                if i == 5 {
                    continue;
                } // disconnect while held
                match i {
                    1 | 4 | 8 => ring.input(1, 106, 1, 0),
                    2 | 7 | 9 => ring.input(1, 106, 2, 0),
                    _ => {}
                }
                let now = if i == 2 { 1001 } else { 0 };
                // Stale causes an additional head request (i3), then fresh press.
                stream
                    .write_all(
                        encode_response(&ring.poll(decode_request(&request).unwrap(), now), now)
                            .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let mut grant = KeypadGrant::new(Some(path.clone()));
        assert!(grant.poll().unwrap().is_empty());
        assert_eq!(grant.poll().unwrap(), vec![Key::Right]);
        assert!(grant.held_keys());
        assert!(grant.poll().unwrap().is_empty());
        assert!(!grant.held_keys());
        assert_eq!(grant.poll().unwrap(), vec![Key::Right]);
        assert!(grant.held_keys());
        assert_eq!(grant.poll(), Err(KeypadFailure::Disconnected));
        assert!(!grant.held_keys());
        assert!(grant.poll().unwrap().is_empty());
        assert!(!grant.held_keys());
        assert!(grant.poll().unwrap().is_empty());
        assert_eq!(grant.poll().unwrap(), vec![Key::Right]);
        assert_eq!(grant.poll().unwrap(), vec![Key::Right]);
        thread.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn denied_without_provider_contact() {
        assert_eq!(KeypadGrant::new(None).poll(), Err(KeypadFailure::Denied));
    }
}
