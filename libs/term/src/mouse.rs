// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! PS/2 mouse packet decoding (the standard three-byte protocol).
//!
//! A mouse in stream mode sends three bytes per report:
//!
//! ```text
//! byte 0: Y overflow | X overflow | Y sign | X sign | 1 | middle | right | left
//! byte 1: X movement, low 8 bits of a 9-bit two's complement number
//! byte 2: Y movement, likewise; positive is away from the user ("up")
//! ```
//!
//! Bytes can be lost, which would shift every later packet. Bit 3 of the
//! first byte is always set, so a byte without it cannot start a packet
//! and is skipped; that resynchronises within a few packets.

/// The left button.
pub const BUTTON_LEFT: u8 = 1 << 0;
/// The right button.
pub const BUTTON_RIGHT: u8 = 1 << 1;
/// The middle button.
pub const BUTTON_MIDDLE: u8 = 1 << 2;

/// One mouse report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Movement to the right, in device units.
    pub dx: i16,
    /// Movement down the screen (the wire value, negated).
    pub dy: i16,
    /// Buttons held: [`BUTTON_LEFT`], [`BUTTON_RIGHT`], [`BUTTON_MIDDLE`].
    pub buttons: u8,
}

/// Assembles reports from the mouse's byte stream.
#[derive(Debug, Default)]
pub struct Decoder {
    packet: [u8; 3],
    filled: usize,
}

impl Decoder {
    /// Creates a decoder waiting for the start of a packet.
    pub const fn new() -> Self {
        Self {
            packet: [0; 3],
            filled: 0,
        }
    }

    /// Processes one byte and returns the report it completes, if any.
    pub fn feed(&mut self, byte: u8) -> Option<Report> {
        if self.filled == 0 && byte & 0x08 == 0 {
            // Cannot be the first byte of a packet.
            return None;
        }
        self.packet[self.filled] = byte;
        self.filled += 1;
        if self.filled < 3 {
            return None;
        }
        self.filled = 0;

        let [flags, x, y] = self.packet;
        if flags & 0xC0 != 0 {
            // Overflow: the movement values are meaningless.
            return None;
        }
        let extend = |low: u8, negative: bool| i16::from(low) - if negative { 256 } else { 0 };
        Some(Report {
            dx: extend(x, flags & 0x10 != 0),
            dy: -extend(y, flags & 0x20 != 0),
            buttons: flags & 0x07,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(decoder: &mut Decoder, bytes: &[u8]) -> std::vec::Vec<Report> {
        bytes
            .iter()
            .filter_map(|&byte| decoder.feed(byte))
            .collect()
    }

    #[test]
    fn decodes_movement_and_buttons() {
        let mut decoder = Decoder::new();
        let reports = feed_all(
            &mut decoder,
            &[
                0x08, 5, 3, // right 5, up 3
                0x18, 0xFB, 0, // left 5
                0x28, 0, 0xFD, // down 3
                0x09, 0, 0, // left button
                0x0E, 0, 0, // right and middle
                0x18, 0x00, 0, // left 256
            ],
        );
        let report = |dx, dy, buttons| Report { dx, dy, buttons };
        assert_eq!(
            reports,
            [
                report(5, -3, 0),
                report(-5, 0, 0),
                report(0, 3, 0),
                report(0, 0, BUTTON_LEFT),
                report(0, 0, BUTTON_RIGHT | BUTTON_MIDDLE),
                report(-256, 0, 0),
            ]
        );
    }

    #[test]
    fn drops_overflowed_packets() {
        let mut decoder = Decoder::new();
        assert!(feed_all(&mut decoder, &[0x48, 1, 1, 0x88, 1, 1]).is_empty());
        assert_eq!(feed_all(&mut decoder, &[0x08, 1, 0]).len(), 1);
    }

    #[test]
    fn resynchronises_after_a_lost_byte() {
        let mut decoder = Decoder::new();
        // The first packet lost its first byte: 2 and 4 cannot start a
        // packet, so they are skipped and the next packet is intact.
        let reports = feed_all(&mut decoder, &[2, 4, 0x08, 7, 0, 0x08, 1, 0]);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].dx, 7);
    }
}
