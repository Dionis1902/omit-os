// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! USB keyboards and mice in their *boot protocol*.
//!
//! Every USB keyboard and mouse can be switched to a fixed, simple report
//! format meant for firmware (USB HID specification, appendix B), which
//! saves parsing report descriptors:
//!
//! * a keyboard report is 8 bytes: a bit mask of the modifier keys, a
//!   reserved byte, and the usage codes of up to six keys held down;
//! * a mouse report is at least 3 bytes: buttons, movement right,
//!   movement down.
//!
//! Keyboard reports say which keys *are* down, so [`BootKeyboard`]
//! compares each report with the previous one and produces what a PC
//! keyboard would have sent — set-1 scancodes for presses and releases —
//! which the existing [`crate::scancode::Decoder`] turns into key
//! presses. One keyboard layout table serves both kinds of keyboard.

use crate::mouse::Report;

/// Size of a boot keyboard report.
pub const KEYBOARD_REPORT: usize = 8;

/// Reported in every key slot when more keys are held than fit.
const ROLL_OVER: u8 = 0x01;
const EXTENDED_PREFIX: u8 = 0xE0;
const RELEASE: u8 = 0x80;

/// Scancodes of the modifier keys, by bit of the report's first byte:
/// left Ctrl, Shift, Alt, GUI, then the right ones. The flag says whether
/// the code is prefixed with `0xE0`.
const MODIFIERS: [(bool, u8); 8] = [
    (false, 0x1D),
    (false, 0x2A),
    (false, 0x38),
    (true, 0x5B),
    (true, 0x1D),
    (false, 0x36),
    (true, 0x38),
    (true, 0x5C),
];

/// The set-1 make code of the key with HID usage code `usage` (keyboard
/// page), and whether it is prefixed with `0xE0`. `None` for keys a PC
/// keyboard's main block does not have, and for the modifier keys, which
/// are not reported by usage code.
pub fn make_code(usage: u8) -> Option<(bool, u8)> {
    /// Usage codes 0x04 (A) to 0x38 (/).
    const MAIN: [u8; 0x35] = [
        // a to z
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C, // 1 to 9, 0
        0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
        // Enter, Escape, Backspace, Tab, Space
        0x1C, 0x01, 0x0E, 0x0F, 0x39, // - = [ ] \ (non-US #) ; ' ` , . /
        0x0C, 0x0D, 0x1A, 0x1B, 0x2B, 0x2B, 0x27, 0x28, 0x29, 0x33, 0x34, 0x35,
    ];
    Some(match usage {
        0x04..=0x38 => (false, MAIN[usize::from(usage - 0x04)]),
        0x39 => (false, 0x3A),                // Caps Lock
        0x3A..=0x43 => (false, usage + 0x01), // F1 to F10
        0x44 => (false, 0x57),                // F11
        0x45 => (false, 0x58),                // F12
        0x49 => (true, 0x52),                 // Insert
        0x4A => (true, 0x47),                 // Home
        0x4B => (true, 0x49),                 // Page Up
        0x4C => (true, 0x53),                 // Delete
        0x4D => (true, 0x4F),                 // End
        0x4E => (true, 0x51),                 // Page Down
        0x4F => (true, 0x4D),                 // Right
        0x50 => (true, 0x4B),                 // Left
        0x51 => (true, 0x50),                 // Down
        0x52 => (true, 0x48),                 // Up
        0x54 => (true, 0x35),                 // keypad /
        0x55 => (false, 0x37),                // keypad *
        0x58 => (true, 0x1C),                 // keypad Enter
        _ => return None,
    })
}

/// Turns boot keyboard reports into scancodes.
#[derive(Debug, Default)]
pub struct BootKeyboard {
    modifiers: u8,
    keys: [u8; 6],
    /// The key pressed most recently that is still held, for auto-repeat.
    latest: Option<u8>,
}

impl BootKeyboard {
    /// A keyboard with no keys held.
    pub const fn new() -> Self {
        Self {
            modifiers: 0,
            keys: [0; 6],
            latest: None,
        }
    }

    /// Processes one report: calls `emit` with each scancode byte for the
    /// keys released and then the keys pressed since the previous report.
    /// Returns whether a key (other than a modifier) was newly pressed.
    /// Short reports and "too many keys" reports are ignored.
    pub fn feed(&mut self, report: &[u8], mut emit: impl FnMut(u8)) -> bool {
        let Some(report) = report.get(..KEYBOARD_REPORT) else {
            return false;
        };
        let keys: [u8; 6] = [
            report[2], report[3], report[4], report[5], report[6], report[7],
        ];
        if keys.contains(&ROLL_OVER) {
            return false;
        }
        let mut send = |(extended, code): (bool, u8), pressed: bool| {
            if extended {
                emit(EXTENDED_PREFIX);
            }
            emit(if pressed { code } else { code | RELEASE });
        };

        let modifiers = report[0];
        for (bit, &scancode) in MODIFIERS.iter().enumerate() {
            let mask = 1 << bit;
            if (modifiers ^ self.modifiers) & mask != 0 {
                send(scancode, modifiers & mask != 0);
            }
        }
        for &usage in self.keys.iter().filter(|&&usage| usage > ROLL_OVER) {
            if !keys.contains(&usage) {
                if let Some(scancode) = make_code(usage) {
                    send(scancode, false);
                }
                if self.latest == Some(usage) {
                    self.latest = None;
                }
            }
        }
        let mut pressed = false;
        for &usage in keys.iter().filter(|&&usage| usage > ROLL_OVER) {
            if !self.keys.contains(&usage)
                && let Some(scancode) = make_code(usage)
            {
                send(scancode, true);
                self.latest = Some(usage);
                pressed = true;
            }
        }
        self.modifiers = modifiers;
        self.keys = keys;
        pressed
    }

    /// Sends the make code of the most recently pressed key again, if it
    /// is still held: what a PC keyboard does by itself and a USB
    /// keyboard leaves to its driver. Returns whether there was one.
    pub fn repeat(&self, mut emit: impl FnMut(u8)) -> bool {
        let Some((extended, code)) = self.latest.and_then(make_code) else {
            return false;
        };
        // Caps Lock toggles on each make code; it must not repeat.
        if code == 0x3A && !extended {
            return false;
        }
        if extended {
            emit(EXTENDED_PREFIX);
        }
        emit(code);
        true
    }
}

/// Decodes a boot mouse report: buttons (bit 0 left, 1 right, 2 middle),
/// movement right, movement down. Longer reports (a wheel, say) are
/// accepted and the rest ignored.
pub fn boot_mouse(report: &[u8]) -> Option<Report> {
    let &[buttons, dx, dy, ..] = report else {
        return None;
    };
    Some(Report {
        dx: i16::from(dx as i8),
        dy: i16::from(dy as i8),
        buttons: buttons & 0x07,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scancode::{Decoder, Key};
    use std::vec::Vec;

    const LEFT_SHIFT: u8 = 0x02;
    const RIGHT_CONTROL: u8 = 0x10;

    fn report(modifiers: u8, keys: &[u8]) -> [u8; 8] {
        let mut report = [0u8; 8];
        report[0] = modifiers;
        report[2..2 + keys.len()].copy_from_slice(keys);
        report
    }

    /// Feeds reports through both stages and collects the key presses.
    fn type_reports(reports: &[[u8; 8]]) -> Vec<Key> {
        let mut keyboard = BootKeyboard::new();
        let mut decoder = Decoder::new();
        let mut keys = Vec::new();
        for report in reports {
            keyboard.feed(report, |scancode| keys.extend(decoder.feed_key(scancode)));
        }
        keys
    }

    fn text(keys: &[Key]) -> Vec<u8> {
        keys.iter()
            .filter_map(|key| match key {
                Key::Char(ch) => Some(*ch),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn typing_with_and_without_shift() {
        // h, e (h still held), release both, Shift+l, l again, o, Enter.
        let keys = type_reports(&[
            report(0, &[0x0B]),
            report(0, &[0x0B, 0x08]),
            report(0, &[]),
            report(LEFT_SHIFT, &[]),
            report(LEFT_SHIFT, &[0x0F]),
            report(0, &[]),
            report(0, &[0x0F]),
            report(0, &[]),
            report(0, &[0x12]),
            report(0, &[0x28]),
            report(0, &[]),
        ]);
        assert_eq!(text(&keys), b"heLlo\r");
    }

    #[test]
    fn every_printable_key_matches_the_pc_layout() {
        // Usage codes in the order of the US layout's rows.
        let usages: Vec<u8> = (0x04..=0x31).chain(0x33..=0x38).collect();
        let plain = type_reports(
            &usages
                .iter()
                .flat_map(|&usage| [report(0, &[usage]), report(0, &[])])
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            text(&plain),
            b"abcdefghijklmnopqrstuvwxyz1234567890\r\x1b\x7f\t -=[]\\;'`,./"
        );
        let shifted = type_reports(
            &usages
                .iter()
                .flat_map(|&usage| [report(LEFT_SHIFT, &[usage]), report(LEFT_SHIFT, &[])])
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            text(&shifted),
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()\r\x1b\x7f\t _+{}|:\"~<>?"
        );
    }

    #[test]
    fn control_navigation_and_function_keys() {
        let keys = type_reports(&[
            // Right Ctrl + c.
            report(RIGHT_CONTROL, &[0x06]),
            report(0, &[]),
            report(0, &[0x52]), // Up
            report(0, &[0x50]), // Left (Up released in the same report)
            report(0, &[0x4C]), // Delete
            report(0, &[0x3A]), // F1
            report(0, &[0x45]), // F12
            report(0, &[0x58]), // keypad Enter
            report(0, &[]),
        ]);
        assert_eq!(
            keys,
            [
                Key::Char(3),
                Key::Up,
                Key::Left,
                Key::Delete,
                Key::Function(1),
                Key::Function(12),
                Key::Char(b'\r'),
            ]
        );
    }

    #[test]
    fn a_held_key_is_reported_once_and_can_repeat() {
        let mut keyboard = BootKeyboard::new();
        let mut codes = Vec::new();
        assert!(keyboard.feed(&report(0, &[0x04]), |code| codes.push(code)));
        // The same report again (keyboards resend while idle): nothing.
        assert!(!keyboard.feed(&report(0, &[0x04]), |code| codes.push(code)));
        assert_eq!(codes, [0x1E]);
        // Auto-repeat sends the make code again, for the newest key.
        assert!(keyboard.repeat(|code| codes.push(code)));
        assert!(keyboard.feed(&report(0, &[0x04, 0x50]), |code| codes.push(code)));
        assert!(keyboard.repeat(|code| codes.push(code)));
        assert_eq!(codes, [0x1E, 0x1E, 0xE0, 0x4B, 0xE0, 0x4B]);
        // Releasing the newest key stops the repeat, even though another
        // key is still down (as PC keyboards do).
        codes.clear();
        keyboard.feed(&report(0, &[0x04]), |code| codes.push(code));
        assert_eq!(codes, [0xE0, 0xCB]);
        assert!(!keyboard.repeat(|_| panic!("repeated a released key")));
        // Caps Lock never repeats; modifiers are not "keys".
        keyboard.feed(&report(0, &[0x39]), |_| {});
        assert!(!keyboard.repeat(|_| panic!("repeated Caps Lock")));
        assert!(!keyboard.feed(&report(LEFT_SHIFT, &[0x39]), |_| {}));
    }

    #[test]
    fn modifiers_produce_their_own_scancodes() {
        let mut keyboard = BootKeyboard::new();
        let mut codes = Vec::new();
        keyboard.feed(&report(0xFF, &[]), |code| codes.push(code));
        assert_eq!(
            codes,
            [
                0x1D, 0x2A, 0x38, 0xE0, 0x5B, 0xE0, 0x1D, 0x36, 0xE0, 0x38, 0xE0, 0x5C
            ]
        );
        codes.clear();
        keyboard.feed(&report(0, &[]), |code| codes.push(code));
        assert_eq!(
            codes,
            [
                0x9D, 0xAA, 0xB8, 0xE0, 0xDB, 0xE0, 0x9D, 0xB6, 0xE0, 0xB8, 0xE0, 0xDC
            ]
        );
    }

    #[test]
    fn bad_reports_change_nothing() {
        let mut keyboard = BootKeyboard::new();
        let mut codes = Vec::new();
        keyboard.feed(&report(0, &[0x04]), |code| codes.push(code));
        codes.clear();
        // Too short, and "too many keys held".
        assert!(!keyboard.feed(&[0, 0, 0x05], |code| codes.push(code)));
        assert!(!keyboard.feed(&report(0, &[1, 1, 1, 1, 1, 1]), |code| codes.push(code)));
        assert!(codes.is_empty());
        // The key from before is still considered held: releasing it
        // produces its break code.
        keyboard.feed(&report(0, &[]), |code| codes.push(code));
        assert_eq!(codes, [0x9E]);
        // Unknown usage codes are ignored without disturbing others.
        codes.clear();
        keyboard.feed(&report(0, &[0xDD, 0x05]), |code| codes.push(code));
        assert_eq!(codes, [0x30]);
    }

    #[test]
    fn mouse_reports() {
        assert_eq!(
            boot_mouse(&[0x01, 5, 0xFD]),
            Some(Report {
                dx: 5,
                dy: -3,
                buttons: 1
            })
        );
        // A wheel byte and unused button bits are ignored.
        assert_eq!(
            boot_mouse(&[0xFA, 0x80, 0x7F, 9]),
            Some(Report {
                dx: -128,
                dy: 127,
                buttons: 2
            })
        );
        assert_eq!(boot_mouse(&[1, 2]), None);
    }
}
