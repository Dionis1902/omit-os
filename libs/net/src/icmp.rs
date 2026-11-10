// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! ICMP echo request and reply (RFC 792): what `ping` sends and what
//! comes back.

use crate::{be16, checksum};

/// Length of the echo header.
pub const HEADER_LEN: usize = 8;

const TYPE_ECHO_REPLY: u8 = 0;
const TYPE_ECHO_REQUEST: u8 = 8;

/// Which way an echo message goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// "Send this back."
    Request,
    /// "Here it is."
    Reply,
}

/// An echo message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Echo<'a> {
    /// Request or reply.
    pub kind: Kind,
    /// Chosen by the requester to tell its conversations apart.
    pub identifier: u16,
    /// Chosen by the requester to tell its requests apart.
    pub sequence: u16,
    /// Data returned unchanged.
    pub payload: &'a [u8],
}

impl<'a> Echo<'a> {
    /// Parses an ICMP message; `None` unless it is an echo request or
    /// reply with a correct checksum.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER_LEN || data[1] != 0 || checksum(data) != 0 {
            return None;
        }
        let kind = match data[0] {
            TYPE_ECHO_REQUEST => Kind::Request,
            TYPE_ECHO_REPLY => Kind::Reply,
            _ => return None,
        };
        Some(Self {
            kind,
            identifier: be16(data, 4),
            sequence: be16(data, 6),
            payload: &data[HEADER_LEN..],
        })
    }

    /// Writes the message at the start of `buffer` and returns its
    /// length, or `None` if the buffer is too small.
    pub fn write(&self, buffer: &mut [u8]) -> Option<usize> {
        let length = HEADER_LEN + self.payload.len();
        let out = buffer.get_mut(..length)?;
        out[0] = match self.kind {
            Kind::Request => TYPE_ECHO_REQUEST,
            Kind::Reply => TYPE_ECHO_REPLY,
        };
        out[1] = 0;
        out[2..4].fill(0);
        out[4..6].copy_from_slice(&self.identifier.to_be_bytes());
        out[6..8].copy_from_slice(&self.sequence.to_be_bytes());
        out[HEADER_LEN..].copy_from_slice(self.payload);
        let sum = checksum(out);
        out[2..4].copy_from_slice(&sum.to_be_bytes());
        Some(length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_echo_round_trips() {
        for payload in [&b""[..], b"x", b"omit os ping"] {
            let echo = Echo {
                kind: Kind::Request,
                identifier: 0x1234,
                sequence: 7,
                payload,
            };
            let mut buffer = [0u8; 32];
            let length = echo.write(&mut buffer).unwrap();
            assert_eq!(length, 8 + payload.len());
            assert_eq!(Echo::parse(&buffer[..length]), Some(echo));
        }
    }

    #[test]
    fn the_bytes_match_a_hand_computed_message() {
        let echo = Echo {
            kind: Kind::Reply,
            identifier: 1,
            sequence: 2,
            payload: &[0xAB, 0xCD],
        };
        let mut buffer = [0u8; 10];
        echo.write(&mut buffer).unwrap();
        // Words 0000 0000 0001 0002 abcd sum to abd0; checksum 542f.
        assert_eq!(buffer, [0, 0, 0x54, 0x2F, 0, 1, 0, 2, 0xAB, 0xCD]);
    }

    #[test]
    fn damaged_and_other_messages_are_refused() {
        let echo = Echo {
            kind: Kind::Request,
            identifier: 9,
            sequence: 9,
            payload: b"abcd",
        };
        let mut buffer = [0u8; 12];
        echo.write(&mut buffer).unwrap();
        for byte in 0..12 {
            let mut bad = buffer;
            bad[byte] ^= 1;
            assert_eq!(Echo::parse(&bad), None, "byte {byte}");
        }
        assert_eq!(Echo::parse(&buffer[..7]), None);
        // Destination unreachable (type 3), with a valid checksum.
        let mut other = [3u8, 0, 0, 0, 0, 0, 0, 0];
        let sum = checksum(&other);
        other[2..4].copy_from_slice(&sum.to_be_bytes());
        assert_eq!(Echo::parse(&other), None);
        assert_eq!(echo.write(&mut [0; 11]), None);
    }
}
