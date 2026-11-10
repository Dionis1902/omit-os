// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Ethernet II frames: destination, source, EtherType, payload. The
//! frame check sequence is the network card's business and never appears
//! here.

use crate::{MacAddr, be16, mac_at};

/// Length of the header.
pub const HEADER_LEN: usize = 14;
/// Shortest frame a card will send, without the check sequence; shorter
/// ones are padded with zeroes.
pub const MIN_FRAME_LEN: usize = 60;
/// Longest frame without the check sequence: header and 1500 bytes.
pub const MAX_FRAME_LEN: usize = 1514;

/// EtherType of IPv4.
pub const TYPE_IPV4: u16 = 0x0800;
/// EtherType of ARP.
pub const TYPE_ARP: u16 = 0x0806;

/// A received frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Who the frame is for.
    pub destination: MacAddr,
    /// Who sent it.
    pub source: MacAddr,
    /// What the payload is.
    pub ethertype: u16,
    /// Everything after the header, padding included.
    pub payload: &'a [u8],
}

impl<'a> Frame<'a> {
    /// Splits `data` into header fields and payload; `None` if it is
    /// shorter than a header.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER_LEN {
            return None;
        }
        Some(Self {
            destination: mac_at(data, 0),
            source: mac_at(data, 6),
            ethertype: be16(data, 12),
            payload: &data[HEADER_LEN..],
        })
    }
}

/// Writes a header at the start of `buffer` and returns its length, or
/// `None` if the buffer is too small.
pub fn write_header(
    buffer: &mut [u8],
    destination: MacAddr,
    source: MacAddr,
    ethertype: u16,
) -> Option<usize> {
    let header = buffer.get_mut(..HEADER_LEN)?;
    header[..6].copy_from_slice(&destination.0);
    header[6..12].copy_from_slice(&source.0);
    header[12..].copy_from_slice(&ethertype.to_be_bytes());
    Some(HEADER_LEN)
}

/// Zero-fills `buffer[length..]` up to the minimum frame length and
/// returns the length to send. `buffer` must be at least
/// [`MIN_FRAME_LEN`] long.
pub fn pad(buffer: &mut [u8], length: usize) -> usize {
    if length >= MIN_FRAME_LEN {
        return length;
    }
    buffer[length..MIN_FRAME_LEN].fill(0);
    MIN_FRAME_LEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_round_trips() {
        let mut buffer = [0xAAu8; 64];
        let source = MacAddr([2, 0, 0, 0, 0, 1]);
        assert_eq!(
            write_header(&mut buffer, MacAddr::BROADCAST, source, TYPE_ARP),
            Some(14)
        );
        let frame = Frame::parse(&buffer[..20]).unwrap();
        assert_eq!(frame.destination, MacAddr::BROADCAST);
        assert_eq!(frame.source, source);
        assert_eq!(frame.ethertype, TYPE_ARP);
        assert_eq!(frame.payload, &[0xAA; 6]);
    }

    #[test]
    fn short_input_and_short_buffers_are_refused() {
        assert_eq!(Frame::parse(&[0; 13]), None);
        assert_eq!(Frame::parse(&[0; 14]).unwrap().payload, &[] as &[u8]);
        assert_eq!(
            write_header(&mut [0; 13], MacAddr::ZERO, MacAddr::ZERO, 0),
            None
        );
    }

    #[test]
    fn short_frames_are_padded_with_zeroes() {
        let mut buffer = [0xAAu8; 64];
        assert_eq!(pad(&mut buffer, 42), 60);
        assert!(buffer[42..60].iter().all(|&b| b == 0));
        assert_eq!(buffer[41], 0xAA);
        assert_eq!(buffer[60], 0xAA);
        assert_eq!(pad(&mut buffer, 61), 61);
    }
}
