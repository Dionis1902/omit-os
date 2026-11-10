// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! PC keyboard scancode decoding (scancode set 1, US layout).
//!
//! Set 1 is what the keyboard controller delivers by default on
//! PC-compatible machines (the controller translates the keyboard's native
//! set 2). Each key sends a "make" code when pressed and the same code with
//! bit 7 set when released; some keys are prefixed with `0xE0`.
//!
//! The decoder produces the bytes a serial terminal would send, so both
//! input paths can feed the same line editor: Enter is carriage return,
//! Backspace is DEL, and Ctrl+letter is the corresponding control code.
//! [`Decoder::feed`] ignores keys without an ASCII meaning;
//! [`Decoder::feed_key`] also reports arrows, navigation and function
//! keys, for programs that want them.

const RELEASE: u8 = 0x80;
const EXTENDED_PREFIX: u8 = 0xE0;

const LEFT_SHIFT: u8 = 0x2A;
const RIGHT_SHIFT: u8 = 0x36;
const CONTROL: u8 = 0x1D;
const ALT: u8 = 0x38;
const CAPS_LOCK: u8 = 0x3A;
const ENTER: u8 = 0x1C;
const KEYPAD_SLASH: u8 = 0x35;

/// Unshifted characters for make codes 0x00-0x39; 0 means "no character".
const PLAIN: [u8; 0x3A] = [
    0, 0x1B, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 0x7F, b'\t',
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', b'\r', 0, b'a', b's',
    b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`', 0, b'\\', b'z', b'x', b'c', b'v',
    b'b', b'n', b'm', b',', b'.', b'/', 0, b'*', 0, b' ',
];

/// Shifted characters for the same codes.
const SHIFTED: [u8; 0x3A] = [
    0, 0x1B, b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(', b')', b'_', b'+', 0x7F, b'\t',
    b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', b'{', b'}', b'\r', 0, b'A', b'S',
    b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':', b'"', b'~', 0, b'|', b'Z', b'X', b'C', b'V',
    b'B', b'N', b'M', b'<', b'>', b'?', 0, b'*', 0, b' ',
];

/// A key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A key with an ASCII meaning, as a serial terminal would send it.
    Char(u8),
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Insert.
    Insert,
    /// Delete (the navigation key, not Backspace).
    Delete,
    /// F1 to F12, as 1 to 12.
    Function(u8),
}

/// Keyboard state: modifier keys and prefix tracking.
#[derive(Debug, Default)]
pub struct Decoder {
    left_shift: bool,
    right_shift: bool,
    control: bool,
    left_alt: bool,
    right_alt: bool,
    caps_lock: bool,
    extended: bool,
}

/// [`Decoder::modifiers`]: a Shift key is held.
pub const MODIFIER_SHIFT: u8 = 1 << 0;
/// [`Decoder::modifiers`]: a Ctrl key is held.
pub const MODIFIER_CONTROL: u8 = 1 << 1;
/// [`Decoder::modifiers`]: an Alt key is held.
pub const MODIFIER_ALT: u8 = 1 << 2;

impl Decoder {
    /// Creates a decoder with no modifiers active.
    pub const fn new() -> Self {
        Self {
            left_shift: false,
            right_shift: false,
            control: false,
            left_alt: false,
            right_alt: false,
            caps_lock: false,
            extended: false,
        }
    }

    /// The modifier keys held after the scancodes fed so far, as
    /// `MODIFIER_*` bits. (One Ctrl key let go counts as both let go:
    /// the two are not told apart.)
    pub fn modifiers(&self) -> u8 {
        let mut held = 0;
        if self.left_shift || self.right_shift {
            held |= MODIFIER_SHIFT;
        }
        if self.control {
            held |= MODIFIER_CONTROL;
        }
        if self.left_alt || self.right_alt {
            held |= MODIFIER_ALT;
        }
        held
    }

    /// Processes one scancode byte and returns the character it produces,
    /// if any.
    pub fn feed(&mut self, scancode: u8) -> Option<u8> {
        match self.feed_key(scancode)? {
            Key::Char(ch) => Some(ch),
            _ => None,
        }
    }

    /// Processes one scancode byte and returns the key press it completes,
    /// if any.
    pub fn feed_key(&mut self, scancode: u8) -> Option<Key> {
        if scancode == EXTENDED_PREFIX {
            self.extended = true;
            return None;
        }
        let extended = core::mem::take(&mut self.extended);
        let pressed = scancode & RELEASE == 0;
        let code = scancode & !RELEASE;

        if extended {
            return match code {
                // Right Ctrl.
                CONTROL => {
                    self.control = pressed;
                    None
                }
                // Right Alt.
                ALT => {
                    self.right_alt = pressed;
                    None
                }
                ENTER if pressed => Some(Key::Char(b'\r')),
                KEYPAD_SLASH if pressed => Some(Key::Char(b'/')),
                0x48 if pressed => Some(Key::Up),
                0x50 if pressed => Some(Key::Down),
                0x4B if pressed => Some(Key::Left),
                0x4D if pressed => Some(Key::Right),
                0x47 if pressed => Some(Key::Home),
                0x4F if pressed => Some(Key::End),
                0x49 if pressed => Some(Key::PageUp),
                0x51 if pressed => Some(Key::PageDown),
                0x52 if pressed => Some(Key::Insert),
                0x53 if pressed => Some(Key::Delete),
                // Releases, and the fake shifts some keyboards send
                // around navigation keys.
                _ => None,
            };
        }

        match code {
            LEFT_SHIFT => self.left_shift = pressed,
            RIGHT_SHIFT => self.right_shift = pressed,
            CONTROL => self.control = pressed,
            ALT => self.left_alt = pressed,
            CAPS_LOCK if pressed => self.caps_lock = !self.caps_lock,
            // F1-F10, F11, F12.
            0x3B..=0x44 if pressed => return Some(Key::Function(code - 0x3A)),
            0x57 | 0x58 if pressed => return Some(Key::Function(code - 0x57 + 11)),
            _ if pressed => return self.translate(code).map(Key::Char),
            _ => {}
        }
        None
    }

    fn translate(&self, code: u8) -> Option<u8> {
        let plain = *PLAIN.get(usize::from(code))?;
        if plain == 0 {
            return None;
        }
        let shift = self.left_shift || self.right_shift;
        // Caps Lock affects letters only, and Shift undoes it.
        let upper = if plain.is_ascii_lowercase() {
            shift != self.caps_lock
        } else {
            shift
        };
        let ch = if upper {
            SHIFTED[usize::from(code)]
        } else {
            plain
        };
        if self.control {
            // Ctrl maps @ A-Z [ \ ] ^ _ (and lowercase letters) to 0-31.
            let upper_ch = ch.to_ascii_uppercase();
            return (b'@'..=b'_').contains(&upper_ch).then_some(upper_ch & 0x1F);
        }
        Some(ch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn decode(codes: &[u8]) -> Vec<u8> {
        let mut decoder = Decoder::new();
        codes.iter().filter_map(|&c| decoder.feed(c)).collect()
    }

    /// Make code followed by break code.
    fn tap(code: u8) -> [u8; 2] {
        [code, code | RELEASE]
    }

    #[test]
    fn modifiers_are_reported_while_held() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.modifiers(), 0);
        // Left Alt down: no key, but held.
        assert_eq!(decoder.feed_key(0x38), None);
        assert_eq!(decoder.modifiers(), MODIFIER_ALT);
        // Tab with Alt held is still Tab.
        assert_eq!(decoder.feed_key(0x0F), Some(Key::Char(b'\t')));
        assert_eq!(decoder.feed_key(0x8F), None);
        // Right Alt (prefixed) down, left up: still held.
        assert_eq!(decoder.feed_key(0xE0), None);
        assert_eq!(decoder.feed_key(0x38), None);
        assert_eq!(decoder.feed_key(0xB8), None);
        assert_eq!(decoder.modifiers(), MODIFIER_ALT);
        // Shift and Ctrl join in.
        decoder.feed_key(0x2A);
        decoder.feed_key(0x1D);
        assert_eq!(
            decoder.modifiers(),
            MODIFIER_ALT | MODIFIER_SHIFT | MODIFIER_CONTROL
        );
        // All let go.
        for code in [0xE0, 0xB8, 0xAA, 0x9D] {
            decoder.feed_key(code);
        }
        assert_eq!(decoder.modifiers(), 0);
        // Alt does not disturb ordinary typing afterwards.
        assert_eq!(decoder.feed_key(0x1E), Some(Key::Char(b'a')));
    }

    #[test]
    fn letters_digits_and_punctuation() {
        // l s space - l Enter
        let mut codes = Vec::new();
        for code in [0x26, 0x1F, 0x39, 0x0C, 0x26, 0x1C] {
            codes.extend(tap(code));
        }
        assert_eq!(decode(&codes), b"ls -l\r");
        assert_eq!(decode(&tap(0x02)), b"1");
        assert_eq!(decode(&tap(0x35)), b"/");
        assert_eq!(decode(&tap(0x0E)), [0x7F], "backspace is DEL");
        assert_eq!(decode(&tap(0x0F)), b"\t");
    }

    #[test]
    fn shift_applies_while_held() {
        // LShift down, 'a', '1', LShift up, 'a'
        assert_eq!(
            decode(&[0x2A, 0x1E, 0x9E, 0x02, 0x82, 0xAA, 0x1E, 0x9E]),
            b"A!a"
        );
        // Right shift, '.' -> '>'
        assert_eq!(decode(&[0x36, 0x34, 0xB4, 0xB6, 0x34]), b">.");
    }

    #[test]
    fn caps_lock_toggles_letters_only() {
        // Caps, 'a', '1', Shift+'a' (shift undoes caps), Caps, 'a'
        assert_eq!(
            decode(&[
                0x3A, 0xBA, 0x1E, 0x9E, 0x02, 0x82, 0x2A, 0x1E, 0x9E, 0xAA, 0x3A, 0xBA, 0x1E
            ]),
            b"A1aa"
        );
    }

    #[test]
    fn control_combinations() {
        // Ctrl+C, Ctrl+U, then plain 'c' after release
        assert_eq!(
            decode(&[0x1D, 0x2E, 0xAE, 0x16, 0x96, 0x9D, 0x2E]),
            [0x03, 0x15, b'c']
        );
        // Ctrl+1 has no control code.
        assert_eq!(decode(&[0x1D, 0x02]), []);
        // Right Ctrl (extended) works too.
        assert_eq!(decode(&[0xE0, 0x1D, 0x2E, 0xE0, 0x9D, 0x2E]), [0x03, b'c']);
    }

    #[test]
    fn extended_keys() {
        // Arrow up (E0 48) is ignored and must not be read as keypad '8'
        // or leave the prefix dangling.
        assert_eq!(decode(&[0xE0, 0x48, 0xE0, 0xC8, 0x1E]), b"a");
        // Keypad Enter and keypad '/'.
        assert_eq!(decode(&[0xE0, 0x1C, 0xE0, 0x9C, 0xE0, 0x35]), b"\r/");
        // Fake shift around navigation keys must not latch Shift.
        assert_eq!(
            decode(&[0xE0, 0x2A, 0xE0, 0x47, 0xE0, 0xC7, 0xE0, 0xAA, 0x1E]),
            b"a"
        );
    }

    #[test]
    fn keys_without_characters_and_unknown_codes() {
        // F1, Alt, key releases, codes beyond the table.
        assert_eq!(
            decode(&[0x3B, 0xBB, 0x38, 0xB8, 0x9E, 0x57, 0x7F, 0xFF]),
            []
        );
        assert_eq!(decode(&tap(0x01)), [0x1B], "Escape");
    }

    #[test]
    fn reports_navigation_and_function_keys() {
        let mut decoder = Decoder::new();
        let mut press = |bytes: &[u8]| bytes.iter().filter_map(|&b| decoder.feed_key(b)).last();
        assert_eq!(press(&[0xE0, 0x48]), Some(Key::Up));
        assert_eq!(press(&[0xE0, 0x50]), Some(Key::Down));
        assert_eq!(press(&[0xE0, 0x4B]), Some(Key::Left));
        assert_eq!(press(&[0xE0, 0x4D]), Some(Key::Right));
        assert_eq!(press(&[0xE0, 0x47]), Some(Key::Home));
        assert_eq!(press(&[0xE0, 0x4F]), Some(Key::End));
        assert_eq!(press(&[0xE0, 0x49]), Some(Key::PageUp));
        assert_eq!(press(&[0xE0, 0x51]), Some(Key::PageDown));
        assert_eq!(press(&[0xE0, 0x53]), Some(Key::Delete));
        assert_eq!(press(&[0x3B]), Some(Key::Function(1)));
        assert_eq!(press(&[0x44]), Some(Key::Function(10)));
        assert_eq!(press(&[0x57]), Some(Key::Function(11)));
        assert_eq!(press(&[0x58]), Some(Key::Function(12)));
        // Releases produce nothing; ordinary keys are characters.
        assert_eq!(press(&[0xE0, 0xC8]), None);
        assert_eq!(press(&[0xBB]), None);
        assert_eq!(press(&[0x1E]), Some(Key::Char(b'a')));
        // The plain-character interface still ignores them.
        assert_eq!(decoder.feed(0xE0), None);
        assert_eq!(decoder.feed(0x48), None);
        assert_eq!(decoder.feed(0x3B), None);
    }
}
