//! Host-side input normalization for `soil:canvas@1.0.0`.
//!
//! Raw platform keycodes stay in the host. This crate maps them to the bounded
//! semantic key set or to one printable Unicode scalar, applies held/repeat
//! rules, and never opens a device. Text should come from the platform text
//! system (`text`) so keyboard layout, dead keys and IME are correct; the
//! Linux/Android/macOS keyboard tables are US-layout fallbacks for hosts that
//! only have physical codes.

#![forbid(unsafe_code)]

pub mod profile;

/// Modifier state that accompanies one Canvas input event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

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
    /// One printable Unicode scalar. This is the layout/IME-correct path.
    Character(char),
    Backspace,
    Delete,
    Tab,
    Escape,
}

impl Key {
    /// Number of stateful held-key slots. `Character` is deliberately not
    /// tracked: text repeats are owned by the platform text system.
    pub const COUNT: usize = 38;

    fn slot(self) -> Option<usize> {
        Some(match self {
            Self::Up => 0,
            Self::Down => 1,
            Self::Left => 2,
            Self::Right => 3,
            Self::Select => 4,
            Self::Back => 5,
            Self::Home => 6,
            Self::Menu => 7,
            Self::Info => 8,
            Self::Number(n) if n <= 9 => 9 + n as usize,
            Self::Number(_) => return None,
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
            Self::Character(_) => return None,
            Self::Backspace => 34,
            Self::Delete => 35,
            Self::Tab => 36,
            Self::Escape => 37,
        })
    }

    /// Directions and text editing repeat while held. A held Call or PTT key
    /// must not emit a stream of presses just because the owner is slow.
    pub fn is_repeatable(self) -> bool {
        matches!(
            self,
            Self::Up | Self::Down | Self::Left | Self::Right | Self::Backspace | Self::Delete
        )
    }

    /// Decode one Linux evdev code with phone/remote semantics: numeric keys
    /// stay `Number`, not text. Desktop keyboards should use
    /// [`Key::from_linux_keyboard`].
    pub fn from_linux_evdev(code: u16) -> Option<Self> {
        Some(match code {
            2..=10 => Self::Number((code - 1) as u8),
            11 => Self::Number(0),
            0x20a => Self::Star,
            0x20b => Self::Hash,
            103 => Self::Up,
            108 => Self::Down,
            105 => Self::Left,
            106 => Self::Right,
            28 | 352 => Self::Select,
            158 => Self::Back,
            102 | 172 => Self::Home,
            139 | 0x244 => Self::Menu,
            0x166 => Self::Info,
            169 => Self::Call,
            0x20c => Self::SoftLeft,
            0x20d => Self::SoftRight,
            0x246 => Self::PushToTalk,
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

    /// Decode one Linux evdev code with desktop-keyboard semantics. Printable
    /// keys become `Character`; this is a US-layout fallback. Prefer
    /// [`Normalizer::text`] when the host can supply the composed character.
    pub fn from_linux_keyboard(code: u16, modifiers: Modifiers) -> Option<Self> {
        let shift = modifiers.shift;
        Some(match code {
            // Letters.
            16 => letter('q', shift),
            17 => letter('w', shift),
            18 => letter('e', shift),
            19 => letter('r', shift),
            20 => letter('t', shift),
            21 => letter('y', shift),
            22 => letter('u', shift),
            23 => letter('i', shift),
            24 => letter('o', shift),
            25 => letter('p', shift),
            30 => letter('a', shift),
            31 => letter('s', shift),
            32 => letter('d', shift),
            33 => letter('f', shift),
            34 => letter('g', shift),
            35 => letter('h', shift),
            36 => letter('j', shift),
            37 => letter('k', shift),
            38 => letter('l', shift),
            44 => letter('z', shift),
            45 => letter('x', shift),
            46 => letter('c', shift),
            47 => letter('v', shift),
            48 => letter('b', shift),
            49 => letter('n', shift),
            50 => letter('m', shift),
            // Digits and their shifted symbols.
            2 => shifted('1', '!', shift),
            3 => shifted('2', '@', shift),
            4 => shifted('3', '#', shift),
            5 => shifted('4', '$', shift),
            6 => shifted('5', '%', shift),
            7 => shifted('6', '^', shift),
            8 => shifted('7', '&', shift),
            9 => shifted('8', '*', shift),
            10 => shifted('9', '(', shift),
            11 => shifted('0', ')', shift),
            // Punctuation.
            12 => shifted('-', '_', shift),
            13 => shifted('=', '+', shift),
            26 => shifted('[', '{', shift),
            27 => shifted(']', '}', shift),
            39 => shifted(';', ':', shift),
            40 => shifted('\'', '"', shift),
            41 => shifted('`', '~', shift),
            43 => shifted('\\', '|', shift),
            51 => shifted(',', '<', shift),
            52 => shifted('.', '>', shift),
            53 => shifted('/', '?', shift),
            57 => Self::Character(' '),
            // Editing and navigation.
            1 => Self::Escape,
            14 => Self::Backspace,
            15 => Self::Tab,
            28 => Self::Select,
            103 => Self::Up,
            108 => Self::Down,
            105 => Self::Left,
            106 => Self::Right,
            102 | 172 => Self::Home,
            111 => Self::Delete,
            _ => return None,
        })
    }

    /// Decode one Android `KeyEvent` keycode with phone/remote semantics.
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
            66 => Self::Select,
            _ => return None,
        })
    }

    /// Decode one Android keycode with desktop-keyboard semantics. Prefer
    /// [`Normalizer::text`] with `KeyEvent.getUnicodeChar()` when available.
    pub fn from_android_keyboard(code: i32, modifiers: Modifiers) -> Option<Self> {
        let shift = modifiers.shift;
        Some(match code {
            // A-Z.
            29 => letter('a', shift),
            30 => letter('b', shift),
            31 => letter('c', shift),
            32 => letter('d', shift),
            33 => letter('e', shift),
            34 => letter('f', shift),
            35 => letter('g', shift),
            36 => letter('h', shift),
            37 => letter('i', shift),
            38 => letter('j', shift),
            39 => letter('k', shift),
            40 => letter('l', shift),
            41 => letter('m', shift),
            42 => letter('n', shift),
            43 => letter('o', shift),
            44 => letter('p', shift),
            45 => letter('q', shift),
            46 => letter('r', shift),
            47 => letter('s', shift),
            48 => letter('t', shift),
            49 => letter('u', shift),
            50 => letter('v', shift),
            51 => letter('w', shift),
            52 => letter('x', shift),
            53 => letter('y', shift),
            54 => letter('z', shift),
            // Digits.
            7 => shifted('0', ')', shift),
            8 => shifted('1', '!', shift),
            9 => shifted('2', '@', shift),
            10 => shifted('3', '#', shift),
            11 => shifted('4', '$', shift),
            12 => shifted('5', '%', shift),
            13 => shifted('6', '^', shift),
            14 => shifted('7', '&', shift),
            15 => shifted('8', '*', shift),
            16 => shifted('9', '(', shift),
            // Punctuation.
            55 => shifted(',', '<', shift),
            56 => shifted('.', '>', shift),
            68 => shifted('`', '~', shift),
            69 => shifted('-', '_', shift),
            70 => shifted('=', '+', shift),
            71 => shifted('[', '{', shift),
            72 => shifted(']', '}', shift),
            73 => shifted('\\', '|', shift),
            74 => shifted(';', ':', shift),
            75 => shifted('\'', '"', shift),
            76 => shifted('/', '?', shift),
            // Editing and navigation.
            61 => Self::Tab,
            62 => Self::Character(' '),
            66 => Self::Select,
            67 => Self::Backspace,
            111 => Self::Escape,
            19 => Self::Up,
            20 => Self::Down,
            21 => Self::Left,
            22 => Self::Right,
            82 => Self::Menu,
            165 => Self::Info,
            _ => return None,
        })
    }

    /// Decode one macOS virtual keycode with desktop-keyboard semantics. Text
    /// should come from the text system; this is a US-layout fallback.
    pub fn from_macos_keyboard(code: u16, modifiers: Modifiers) -> Option<Self> {
        let shift = modifiers.shift;
        Some(match code {
            // Letters.
            0 => letter('a', shift),
            1 => letter('s', shift),
            2 => letter('d', shift),
            3 => letter('f', shift),
            4 => letter('h', shift),
            5 => letter('g', shift),
            6 => letter('z', shift),
            7 => letter('x', shift),
            8 => letter('c', shift),
            9 => letter('v', shift),
            11 => letter('b', shift),
            12 => letter('q', shift),
            13 => letter('w', shift),
            14 => letter('e', shift),
            15 => letter('r', shift),
            16 => letter('y', shift),
            17 => letter('t', shift),
            31 => letter('o', shift),
            32 => letter('u', shift),
            34 => letter('i', shift),
            35 => letter('p', shift),
            37 => letter('l', shift),
            38 => letter('j', shift),
            40 => letter('k', shift),
            45 => letter('n', shift),
            46 => letter('m', shift),
            // Digits and shifted symbols.
            18 => shifted('1', '!', shift),
            19 => shifted('2', '@', shift),
            20 => shifted('3', '#', shift),
            21 => shifted('4', '$', shift),
            22 => shifted('6', '^', shift),
            23 => shifted('5', '%', shift),
            25 => shifted('9', '(', shift),
            26 => shifted('7', '&', shift),
            27 => shifted('-', '_', shift),
            28 => shifted('8', '*', shift),
            29 => shifted('0', ')', shift),
            24 => shifted('=', '+', shift),
            // Punctuation.
            30 => shifted(']', '}', shift),
            33 => shifted('[', '{', shift),
            39 => shifted('\'', '"', shift),
            41 => shifted(';', ':', shift),
            42 => shifted('\\', '|', shift),
            43 => shifted(',', '<', shift),
            44 => shifted('/', '?', shift),
            47 => shifted('.', '>', shift),
            50 => shifted('`', '~', shift),
            49 => Self::Character(' '),
            // Editing and navigation.
            36 => Self::Select,
            48 => Self::Tab,
            51 => Self::Backspace,
            53 => Self::Escape,
            117 => Self::Delete,
            123 => Self::Left,
            124 => Self::Right,
            125 => Self::Down,
            126 => Self::Up,
            115 => Self::Home,
            _ => return None,
        })
    }
}

fn letter(lower: char, shift: bool) -> Key {
    Key::Character(if shift {
        lower.to_ascii_uppercase()
    } else {
        lower
    })
}

fn shifted(lower: char, upper: char, shift: bool) -> Key {
    Key::Character(if shift { upper } else { lower })
}

/// One host keyboard action, normalized from the platform event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Press,
    Release,
    Repeat,
}

/// One normalized Canvas input, matching `soil:canvas`'s `event` record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub key: Key,
    pub modifiers: Modifiers,
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

    /// Apply one already-decoded key. Returns an event only when it should be
    /// delivered to the owner. Repeated non-repeatable keys are dropped.
    pub fn apply(&mut self, key: Key, modifiers: Modifiers, action: Action) -> Option<Event> {
        if let Key::Number(n) = key
            && n > 9
        {
            return None;
        }
        if matches!(key, Key::Character(ch) if ch.is_control()) {
            return None;
        }
        if let Key::Character(_) = key {
            return (action == Action::Press).then_some(Event { key, modifiers });
        }
        let index = key.slot()?;
        let deliver = match action {
            Action::Release => {
                self.held[index] = false;
                false
            }
            Action::Press if !self.held[index] => {
                self.held[index] = true;
                true
            }
            Action::Repeat if self.held[index] && key.is_repeatable() => true,
            _ => false,
        };
        deliver.then_some(Event { key, modifiers })
    }

    /// Deliver one already-composed printable character. This is the preferred
    /// path: the platform text system owns layout, dead keys and IME.
    pub fn text(&mut self, ch: char, modifiers: Modifiers) -> Option<Event> {
        self.apply(Key::Character(ch), modifiers, Action::Press)
    }

    /// Decode and apply a Linux evdev `(code, value)` pair with phone/remote
    /// semantics. `value`: 0 release, 1 press, 2 repeat.
    pub fn apply_linux(&mut self, code: u16, value: i32, modifiers: Modifiers) -> Option<Event> {
        self.apply(
            Key::from_linux_evdev(code)?,
            modifiers,
            action_from_linux(value)?,
        )
    }

    /// Decode and apply a Linux evdev pair with desktop-keyboard semantics.
    pub fn apply_linux_keyboard(
        &mut self,
        code: u16,
        value: i32,
        modifiers: Modifiers,
    ) -> Option<Event> {
        self.apply(
            Key::from_linux_keyboard(code, modifiers)?,
            modifiers,
            action_from_linux(value)?,
        )
    }

    /// Decode and apply an Android keycode with phone/remote semantics.
    pub fn apply_android(
        &mut self,
        code: i32,
        action: Action,
        modifiers: Modifiers,
    ) -> Option<Event> {
        self.apply(Key::from_android(code)?, modifiers, action)
    }

    /// Decode and apply an Android keycode with desktop-keyboard semantics.
    pub fn apply_android_keyboard(
        &mut self,
        code: i32,
        action: Action,
        modifiers: Modifiers,
    ) -> Option<Event> {
        self.apply(
            Key::from_android_keyboard(code, modifiers)?,
            modifiers,
            action,
        )
    }

    /// Decode and apply a macOS virtual keycode with desktop-keyboard semantics.
    pub fn apply_macos(
        &mut self,
        code: u16,
        action: Action,
        modifiers: Modifiers,
    ) -> Option<Event> {
        self.apply(
            Key::from_macos_keyboard(code, modifiers)?,
            modifiers,
            action,
        )
    }

    pub fn is_held(&self, key: Key) -> bool {
        key.slot().is_some_and(|index| self.held[index])
    }
}

fn action_from_linux(value: i32) -> Option<Action> {
    Some(match value {
        0 => Action::Release,
        1 => Action::Press,
        2 => Action::Repeat,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Modifiers {
        Modifiers::default()
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn s22_linux_keymap_decodes_every_physical_group() {
        assert_eq!(Key::from_linux_evdev(2), Some(Key::Number(1)));
        assert_eq!(Key::from_linux_evdev(10), Some(Key::Number(9)));
        assert_eq!(Key::from_linux_evdev(11), Some(Key::Number(0)));
        assert_eq!(Key::from_linux_evdev(0x20a), Some(Key::Star));
        assert_eq!(Key::from_linux_evdev(0x20b), Some(Key::Hash));
        assert_eq!(Key::from_linux_evdev(103), Some(Key::Up));
        assert_eq!(Key::from_linux_evdev(28), Some(Key::Select));
        assert_eq!(Key::from_linux_evdev(158), Some(Key::Back));
        assert_eq!(Key::from_linux_evdev(172), Some(Key::Home));
        assert_eq!(Key::from_linux_evdev(0x244), Some(Key::Menu));
        assert_eq!(Key::from_linux_evdev(0x20c), Some(Key::SoftLeft));
        assert_eq!(Key::from_linux_evdev(0x20d), Some(Key::SoftRight));
        assert_eq!(Key::from_linux_evdev(0x246), Some(Key::PushToTalk));
        assert_eq!(Key::from_linux_evdev(14), None);
        assert_eq!(Key::from_linux_evdev(999), None);
    }

    #[test]
    fn desktop_linux_keyboard_supports_ascii_and_shift() {
        assert_eq!(
            Key::from_linux_keyboard(30, none()),
            Some(Key::Character('a'))
        );
        assert_eq!(
            Key::from_linux_keyboard(30, shift()),
            Some(Key::Character('A'))
        );
        assert_eq!(
            Key::from_linux_keyboard(2, none()),
            Some(Key::Character('1'))
        );
        assert_eq!(
            Key::from_linux_keyboard(2, shift()),
            Some(Key::Character('!'))
        );
        assert_eq!(
            Key::from_linux_keyboard(57, none()),
            Some(Key::Character(' '))
        );
        assert_eq!(Key::from_linux_keyboard(14, none()), Some(Key::Backspace));
        assert_eq!(Key::from_linux_keyboard(15, none()), Some(Key::Tab));
        assert_eq!(Key::from_linux_keyboard(1, none()), Some(Key::Escape));
        assert_eq!(Key::from_linux_keyboard(111, none()), Some(Key::Delete));
    }

    #[test]
    fn android_remote_and_keyboard_map_without_raw_codes() {
        assert_eq!(Key::from_android(19), Some(Key::Up));
        assert_eq!(Key::from_android(23), Some(Key::Select));
        assert_eq!(Key::from_android(4), Some(Key::Back));
        assert_eq!(Key::from_android(7), Some(Key::Number(0)));
        assert_eq!(Key::from_android(16), Some(Key::Number(9)));
        assert_eq!(Key::from_android(85), Some(Key::PlayPause));
        assert_eq!(Key::from_android(999), None);

        assert_eq!(
            Key::from_android_keyboard(29, none()),
            Some(Key::Character('a'))
        );
        assert_eq!(
            Key::from_android_keyboard(54, shift()),
            Some(Key::Character('Z'))
        );
        assert_eq!(
            Key::from_android_keyboard(62, none()),
            Some(Key::Character(' '))
        );
        assert_eq!(Key::from_android_keyboard(67, none()), Some(Key::Backspace));
    }

    #[test]
    fn macos_keyboard_maps_specials_and_ascii() {
        assert_eq!(
            Key::from_macos_keyboard(0, none()),
            Some(Key::Character('a'))
        );
        assert_eq!(
            Key::from_macos_keyboard(0, shift()),
            Some(Key::Character('A'))
        );
        assert_eq!(
            Key::from_macos_keyboard(18, shift()),
            Some(Key::Character('!'))
        );
        assert_eq!(Key::from_macos_keyboard(36, none()), Some(Key::Select));
        assert_eq!(Key::from_macos_keyboard(51, none()), Some(Key::Backspace));
        assert_eq!(Key::from_macos_keyboard(117, none()), Some(Key::Delete));
        assert_eq!(Key::from_macos_keyboard(53, none()), Some(Key::Escape));
        assert_eq!(Key::from_macos_keyboard(123, none()), Some(Key::Left));
        assert_eq!(Key::from_macos_keyboard(124, none()), Some(Key::Right));
        assert_eq!(Key::from_macos_keyboard(125, none()), Some(Key::Down));
        assert_eq!(Key::from_macos_keyboard(126, none()), Some(Key::Up));
    }

    #[test]
    fn text_preserves_unicode_and_modifiers() {
        let mut n = Normalizer::new();
        let event = n.text('é', none()).unwrap();
        assert_eq!(event.key, Key::Character('é'));
        assert_eq!(event.modifiers, none());
        let event = n
            .text(
                '日',
                Modifiers {
                    control: true,
                    ..none()
                },
            )
            .unwrap();
        assert_eq!(event.key, Key::Character('日'));
        assert!(event.modifiers.control);
        let event = n.text('🙂', none()).unwrap();
        assert_eq!(event.key, Key::Character('🙂'));
        assert_eq!(n.text('\n', none()), None);
    }

    #[test]
    fn press_repeat_release_is_bounded_and_text_does_not_repeat() {
        let mut n = Normalizer::new();
        assert_eq!(
            n.apply(Key::Down, none(), Action::Press).map(|e| e.key),
            Some(Key::Down)
        );
        assert_eq!(n.apply(Key::Down, none(), Action::Press), None);
        assert_eq!(
            n.apply(Key::Down, none(), Action::Repeat).map(|e| e.key),
            Some(Key::Down)
        );
        assert_eq!(n.apply(Key::Down, none(), Action::Release), None);
        assert_eq!(n.apply(Key::Down, none(), Action::Repeat), None);

        assert_eq!(
            n.apply(Key::Character('a'), none(), Action::Press)
                .map(|e| e.key),
            Some(Key::Character('a'))
        );
        assert_eq!(n.apply(Key::Character('a'), none(), Action::Repeat), None);
        assert_eq!(n.apply(Key::Character('a'), none(), Action::Release), None);
    }

    #[test]
    fn linux_and_android_helpers_drive_the_same_normalizer() {
        let mut n = Normalizer::new();
        assert_eq!(
            n.apply_linux(106, 1, none()).map(|e| e.key),
            Some(Key::Right)
        );
        assert_eq!(
            n.apply_linux(106, 2, none()).map(|e| e.key),
            Some(Key::Right)
        );
        assert_eq!(n.apply_linux(106, 0, none()), None);
        assert_eq!(
            n.apply_android(22, Action::Press, none()).map(|e| e.key),
            Some(Key::Right)
        );
        assert_eq!(n.apply_android(22, Action::Release, none()), None);
        assert_eq!(
            n.apply_linux(580, 1, none()).map(|e| e.key),
            Some(Key::Menu)
        );
        assert_eq!(n.apply_linux(999, 1, none()), None);
    }

    #[test]
    fn resync_clears_held_state_without_a_release() {
        let mut n = Normalizer::new();
        assert_eq!(
            n.apply(Key::Up, none(), Action::Press).map(|e| e.key),
            Some(Key::Up)
        );
        assert!(n.is_held(Key::Up));
        n.clear();
        assert!(!n.is_held(Key::Up));
        assert_eq!(n.apply(Key::Up, none(), Action::Repeat), None);
        assert_eq!(
            n.apply(Key::Up, none(), Action::Press).map(|e| e.key),
            Some(Key::Up)
        );
    }

    #[test]
    fn out_of_range_digits_are_rejected() {
        let mut n = Normalizer::new();
        assert_eq!(n.apply(Key::Number(10), none(), Action::Press), None);
        assert_eq!(n.apply(Key::Number(255), none(), Action::Press), None);
        assert_eq!(
            n.apply(Key::Number(9), none(), Action::Press)
                .map(|e| e.key),
            Some(Key::Number(9))
        );
    }
}
