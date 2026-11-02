//! Host-side key normalization for `soil:canvas@1.0.0`.
//!
//! Raw Linux evdev and Android keycodes stay in the host. This crate maps them
//! to the bounded semantic `key` variant, applies press/repeat/release rules,
//! and never opens a device. It is intentionally independent of wasmtime and of
//! any particular Canvas renderer.

#![forbid(unsafe_code)]

/// One normalized key delivered to the Canvas owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Select,
    Back,
    Home,
    Menu,
    Info,
    /// 0..=9. Values outside that range are rejected by [`Normalizer::apply`].
    Number(u8),
    Star,
    Hash,
    Call,
    EndCall,
    PushToTalk,
    SoftLeft,
    SoftRight,
    PlayPause,
    Rewind,
    FastForward,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl Key {
    /// Number of distinct held-key slots. Ten are reserved for digits.
    pub const COUNT: usize = 34;

    /// Stable index used for held-state tracking.
    pub fn index(self) -> usize {
        match self {
            Self::Up => 0,
            Self::Down => 1,
            Self::Left => 2,
            Self::Right => 3,
            Self::Select => 4,
            Self::Back => 5,
            Self::Home => 6,
            Self::Menu => 7,
            Self::Info => 8,
            Self::Number(n) => 9 + n as usize,
            Self::Star => 19,
            Self::Hash => 20,
            Self::Call => 21,
            Self::EndCall => 22,
            Self::PushToTalk => 23,
            Self::SoftLeft => 24,
            Self::SoftRight => 25,
            Self::PlayPause => 26,
            Self::Rewind => 27,
            Self::FastForward => 28,
            Self::Next => 29,
            Self::Previous => 30,
            Self::VolumeUp => 31,
            Self::VolumeDown => 32,
            Self::Mute => 33,
        }
    }

    /// Only directions repeat while held. A held Call or PTT key must not emit
    /// a stream of presses just because the owner is slow to drain them.
    pub fn is_repeatable(self) -> bool {
        matches!(self, Self::Up | Self::Down | Self::Left | Self::Right)
    }

    /// Decode one Linux evdev keycode. Unknown keys stay host-side and return
    /// `None`; they are never guessed into a semantic action.
    pub fn from_linux_evdev(code: u16) -> Option<Self> {
        Some(match code {
            // Digit row and phone keys.
            2..=10 => Self::Number((code - 1) as u8),
            11 => Self::Number(0),
            0x20a => Self::Star,
            0x20b => Self::Hash,
            // D-pad and owner navigation.
            103 => Self::Up,
            108 => Self::Down,
            105 => Self::Left,
            106 => Self::Right,
            28 | 352 => Self::Select,
            158 => Self::Back,
            172 => Self::Home,
            139 | 0x244 => Self::Menu,
            0x166 => Self::Info,
            // Phone / flip keys.
            169 => Self::Call,
            0x20c => Self::SoftLeft,
            0x20d => Self::SoftRight,
            0x246 => Self::PushToTalk,
            // Media and volume.
            113 | 248 => Self::Mute,
            114 => Self::VolumeDown,
            115 => Self::VolumeUp,
            163 => Self::Next,
            164 => Self::PlayPause,
            165 => Self::Previous,
            168 => Self::Rewind,
            208 => Self::FastForward,
            _ => return None,
        })
    }

    /// Decode one Android `KeyEvent` keycode. The caller supplies the action.
    pub fn from_android(code: i32) -> Option<Self> {
        Some(match code {
            1 => Self::SoftLeft,
            2 => Self::SoftRight,
            3 => Self::Home,
            4 => Self::Back,
            5 => Self::Call,
            6 => Self::EndCall,
            7..=16 => Self::Number((code - 7) as u8),
            17 => Self::Star,
            18 => Self::Hash,
            19 => Self::Up,
            20 => Self::Down,
            21 => Self::Left,
            22 => Self::Right,
            23 => Self::Select,
            24 => Self::VolumeUp,
            25 => Self::VolumeDown,
            79 => Self::PushToTalk,
            82 => Self::Menu,
            85 => Self::PlayPause,
            87 => Self::Next,
            88 => Self::Previous,
            89 => Self::Rewind,
            90 => Self::FastForward,
            164 => Self::Mute,
            165 => Self::Info,
            _ => return None,
        })
    }
}

/// One host keyboard action, normalized from the platform event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Press,
    Release,
    Repeat,
}

/// Bounded held-key state shared by every Canvas host adapter.
#[derive(Debug)]
pub struct Normalizer {
    held: [bool; Key::COUNT],
}

impl Default for Normalizer {
    fn default() -> Self {
        Self {
            held: [false; Key::COUNT],
        }
    }
}

impl Normalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every held key without inventing releases. Use on transport
    /// loss/resync so a reconnect cannot replay a stale press.
    pub fn clear(&mut self) {
        self.held.fill(false);
    }

    /// Apply one already-decoded key. Returns the key only when it should be
    /// delivered to the owner. Repeated non-directional keys are dropped.
    pub fn apply(&mut self, key: Key, action: Action) -> Option<Key> {
        if let Key::Number(n) = key
            && n > 9
        {
            return None;
        }
        let index = key.index();
        match action {
            Action::Release => {
                self.held[index] = false;
                None
            }
            Action::Press if !self.held[index] => {
                self.held[index] = true;
                Some(key)
            }
            Action::Repeat if self.held[index] && key.is_repeatable() => Some(key),
            _ => None,
        }
    }

    /// Decode and apply a Linux evdev `(code, value)` pair.
    ///
    /// `value`: 0 release, 1 press, 2 repeat. Other values are ignored.
    pub fn apply_linux(&mut self, code: u16, value: i32) -> Option<Key> {
        let action = match value {
            0 => Action::Release,
            1 => Action::Press,
            2 => Action::Repeat,
            _ => return None,
        };
        self.apply(Key::from_linux_evdev(code)?, action)
    }

    /// Decode and apply an Android keycode with an already-normalized action.
    pub fn apply_android(&mut self, code: i32, action: Action) -> Option<Key> {
        self.apply(Key::from_android(code)?, action)
    }

    pub fn is_held(&self, key: Key) -> bool {
        let index = key.index();
        index < Key::COUNT && self.held[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s22_linux_keymap_decodes_every_physical_group() {
        // Digit/star/pound rows from cat-s22-keypad.dtsi.
        assert_eq!(Key::from_linux_evdev(2), Some(Key::Number(1)));
        assert_eq!(Key::from_linux_evdev(10), Some(Key::Number(9)));
        assert_eq!(Key::from_linux_evdev(11), Some(Key::Number(0)));
        assert_eq!(Key::from_linux_evdev(0x20a), Some(Key::Star));
        assert_eq!(Key::from_linux_evdev(0x20b), Some(Key::Hash));
        // D-pad, OK, Back, Home and the orange/app-select key.
        assert_eq!(Key::from_linux_evdev(103), Some(Key::Up));
        assert_eq!(Key::from_linux_evdev(28), Some(Key::Select));
        assert_eq!(Key::from_linux_evdev(158), Some(Key::Back));
        assert_eq!(Key::from_linux_evdev(172), Some(Key::Home));
        assert_eq!(Key::from_linux_evdev(0x244), Some(Key::Menu));
        // Phone soft keys and talk key.
        assert_eq!(Key::from_linux_evdev(0x20c), Some(Key::SoftLeft));
        assert_eq!(Key::from_linux_evdev(0x20d), Some(Key::SoftRight));
        assert_eq!(Key::from_linux_evdev(0x246), Some(Key::PushToTalk));
        // Raw edit/unknown keys stay host-side.
        assert_eq!(Key::from_linux_evdev(14), None);
        assert_eq!(Key::from_linux_evdev(999), None);
    }

    #[test]
    fn android_remote_and_phone_keys_map_without_raw_codes() {
        assert_eq!(Key::from_android(19), Some(Key::Up));
        assert_eq!(Key::from_android(23), Some(Key::Select));
        assert_eq!(Key::from_android(4), Some(Key::Back));
        assert_eq!(Key::from_android(7), Some(Key::Number(0)));
        assert_eq!(Key::from_android(16), Some(Key::Number(9)));
        assert_eq!(Key::from_android(17), Some(Key::Star));
        assert_eq!(Key::from_android(18), Some(Key::Hash));
        assert_eq!(Key::from_android(85), Some(Key::PlayPause));
        assert_eq!(Key::from_android(164), Some(Key::Mute));
        assert_eq!(Key::from_android(999), None);
    }

    #[test]
    fn press_repeat_release_is_bounded_and_directions_only_repeat() {
        let mut n = Normalizer::new();
        assert_eq!(n.apply(Key::Down, Action::Press), Some(Key::Down));
        assert_eq!(n.apply(Key::Down, Action::Press), None);
        assert_eq!(n.apply(Key::Down, Action::Repeat), Some(Key::Down));
        assert_eq!(n.apply(Key::Down, Action::Release), None);
        assert_eq!(n.apply(Key::Down, Action::Repeat), None);

        assert_eq!(
            n.apply(Key::PushToTalk, Action::Press),
            Some(Key::PushToTalk)
        );
        assert_eq!(n.apply(Key::PushToTalk, Action::Repeat), None);
        assert_eq!(n.apply(Key::PushToTalk, Action::Release), None);
    }

    #[test]
    fn linux_and_android_helpers_drive_the_same_normalizer() {
        let mut n = Normalizer::new();
        assert_eq!(n.apply_linux(106, 1), Some(Key::Right));
        assert_eq!(n.apply_linux(106, 2), Some(Key::Right));
        assert_eq!(n.apply_linux(106, 0), None);
        assert_eq!(n.apply_android(22, Action::Press), Some(Key::Right));
        assert_eq!(n.apply_android(22, Action::Release), None);
        assert_eq!(n.apply_linux(580, 1), Some(Key::Menu));
        assert_eq!(n.apply_linux(999, 1), None);
    }

    #[test]
    fn resync_clears_held_state_without_a_release() {
        let mut n = Normalizer::new();
        assert_eq!(n.apply(Key::Up, Action::Press), Some(Key::Up));
        assert!(n.is_held(Key::Up));
        n.clear();
        assert!(!n.is_held(Key::Up));
        assert_eq!(n.apply(Key::Up, Action::Repeat), None);
        assert_eq!(n.apply(Key::Up, Action::Press), Some(Key::Up));
    }

    #[test]
    fn out_of_range_digits_are_rejected() {
        let mut n = Normalizer::new();
        assert_eq!(n.apply(Key::Number(10), Action::Press), None);
        assert_eq!(n.apply(Key::Number(255), Action::Press), None);
        assert_eq!(n.apply(Key::Number(9), Action::Press), Some(Key::Number(9)));
    }
}
