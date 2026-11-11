//! Per-device physical keymaps.
//!
//! [`Key::from_linux_evdev`](crate::Key::from_linux_evdev) is the *union* of every
//! phone key we know about. As a fallback that is right; as a device map it is
//! wrong, because it hands a phone keys it does not have and hides the ones it
//! does have behind "unmapped". A profile says what one specific device sends,
//! so an unmapped key there is a capture bug — visible — instead of a silent
//! missing feature.
//!
//! Codes marked `TODO(capture)` are known to exist on the hardware but have not
//! been read off it yet: run `getevent -l` (Android) or `evtest` (Linux) while
//! pressing every key, then fill the line in. Nothing here is guessed, because a
//! plausible-looking wrong code is the most expensive kind of failure — it maps
//! a key to something adjacent and nobody notices until a user does.

use crate::Key;

/// One device's physical keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceProfile {
    /// CAT S22 Flip — the rugged Android flip with a dedicated PTT key.
    S22,
    /// TCL Flip 2 (4058L) — the QVGA feature phone the ohmOS spec targets.
    TclFlip,
}

impl DeviceProfile {
    /// The profile's stable identifier, for manifests and logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::S22 => "cat-s22",
            Self::TclFlip => "tcl-flip-4058l",
        }
    }

    /// Decode one Linux evdev code for this device.
    pub fn map(self, code: u16) -> Option<Key> {
        match self {
            Self::S22 => s22(code),
            Self::TclFlip => tcl_flip(code),
        }
    }
}

/// CAT S22 Flip.
///
/// The directional-pad codes come from `cat-s22-keypad.dtsi` in the S22 kernel
/// reversal, which `soil-runtime`'s keypad normalizer already cites.
fn s22(code: u16) -> Option<Key> {
    Some(match code {
        103 => Key::Up,
        108 => Key::Down,
        105 => Key::Left,
        106 => Key::Right,
        28 => Key::Select,
        158 => Key::Back,
        172 => Key::Home,
        139 | 0x244 => Key::Menu,

        // Soft keys either side of the d-pad.
        0x20c => Key::SoftLeft,
        0x20d => Key::SoftRight,

        // Numeric keypad. Standard Linux codes; the S22 matrix uses standard
        // codes for everything captured so far.
        2..=10 => Key::Number((code - 1) as u8),
        11 => Key::Number(0),
        0x20a => Key::Star,
        0x20b => Key::Hash,

        // Dedicated PTT key — the reason this profile exists.
        0x246 => Key::PushToTalk,

        // Volume rocker and mute.
        114 => Key::VolumeDown,
        115 => Key::VolumeUp,
        113 | 248 => Key::Mute,

        // TODO(capture): `Clear`. The key exists on the S22 but its code has not
        // been read off the device. When it is, decide the semantic: `Backspace`
        // is the likely answer (it deletes a character in text and acts as Back
        // elsewhere in ohmOS), and `soil:input` has a `named(string)` escape
        // hatch if a first-class `clear` is wanted instead.
        _ => return None,
    })
}

/// TCL Flip 2 (4058L).
///
/// A conventional Android flip keypad: numbers, star/hash, a d-pad, two soft
/// keys, and call/end. No PTT.
fn tcl_flip(code: u16) -> Option<Key> {
    Some(match code {
        103 => Key::Up,
        108 => Key::Down,
        105 => Key::Left,
        106 => Key::Right,
        28 | 352 => Key::Select,
        158 => Key::Back,

        2..=10 => Key::Number((code - 1) as u8),
        11 => Key::Number(0),
        0x20a => Key::Star,
        0x20b => Key::Hash,

        169 => Key::Call,

        // Soft keys either side of the d-pad.
        0x20c => Key::SoftLeft,
        0x20d => Key::SoftRight,

        // TODO(capture): the red End key (`EndCall`) and `Clear`. Both are on
        // the device; neither code has been read off it yet. `Key::EndCall`
        // exists in the vocabulary but is currently unreachable from any evdev
        // code in any table — this is where it should start working.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s22_has_its_documented_keys() {
        let p = DeviceProfile::S22;
        assert_eq!(p.map(103), Some(Key::Up));
        assert_eq!(p.map(28), Some(Key::Select));
        assert_eq!(p.map(158), Some(Key::Back));
        assert_eq!(p.map(0x244), Some(Key::Menu));
        assert_eq!(p.map(0x20c), Some(Key::SoftLeft));
        assert_eq!(p.map(0x20d), Some(Key::SoftRight));
        assert_eq!(p.map(0x246), Some(Key::PushToTalk));
        assert_eq!(p.map(114), Some(Key::VolumeDown));
        assert_eq!(p.map(115), Some(Key::VolumeUp));
        assert_eq!(p.map(2), Some(Key::Number(1)));
        assert_eq!(p.map(11), Some(Key::Number(0)));
        assert_eq!(p.name(), "cat-s22");
    }

    #[test]
    fn tcl_flip_has_its_keypad_and_no_ptt() {
        let p = DeviceProfile::TclFlip;
        assert_eq!(p.map(2), Some(Key::Number(1)));
        assert_eq!(p.map(0x20a), Some(Key::Star));
        assert_eq!(p.map(0x20b), Some(Key::Hash));
        assert_eq!(p.map(169), Some(Key::Call));
        assert_eq!(p.map(0x20c), Some(Key::SoftLeft));
        assert_eq!(p.map(0x20d), Some(Key::SoftRight));
        // The union table maps 0x246 to PushToTalk; this profile must not.
        assert_eq!(p.map(0x246), None);
        assert_eq!(p.name(), "tcl-flip-4058l");
    }

    #[test]
    fn unknown_codes_do_not_leak_between_profiles() {
        // A key that neither device has stays unmapped on both.
        assert_eq!(DeviceProfile::S22.map(0x163), None);
        assert_eq!(DeviceProfile::TclFlip.map(0x163), None);
    }

    #[test]
    fn every_captured_key_round_trips_through_the_union_table() {
        // The profiles are a subset of the union: if the union loses a key, the
        // profiles break too. Keep them consistent.
        for profile in [DeviceProfile::S22, DeviceProfile::TclFlip] {
            for code in [2u16, 11, 103, 108, 105, 106, 158, 0x20a, 0x20b] {
                assert_eq!(
                    profile.map(code),
                    Key::from_linux_evdev(code),
                    "{}: code {code} disagrees with the union table",
                    profile.name()
                );
            }
        }
    }
}
