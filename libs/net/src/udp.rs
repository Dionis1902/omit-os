// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! UDP datagrams over IPv4 (RFC 768).

use crate::ipv4::PROTOCOL_UDP;
use crate::{Ipv4Addr, be16, checksum_add, checksum_finish};

/// Length of the header.
pub const HEADER_LEN: usize = 8;

/// A received datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Datagram<'a> {
    /// Port of the sender.
    pub source_port: u16,
    /// Port of the recipient.
    pub destination_port: u16,
    /// The data.
    pub payload: &'a [u8],
}

/// The checksum's "pseudo header": addresses, protocol and UDP length.
fn pseudo_header_sum(source: Ipv4Addr, destination: Ipv4Addr, length: u16) -> u32 {
    let sum = checksum_add(0, &source.0);
    let sum = checksum_add(sum, &destination.0);
    let sum = checksum_add(sum, &[0, PROTOCOL_UDP]);
    checksum_add(sum, &length.to_be_bytes())
}

impl<'a> Datagram<'a> {
    /// Parses the payload of an IPv4 packet from `source` to
    /// `destination`. `None` if it is shorter than it claims or its
    /// checksum is wrong; a checksum of zero means the sender did not
    /// compute one, which IPv4 allows.
    pub fn parse(data: &'a [u8], source: Ipv4Addr, destination: Ipv4Addr) -> Option<Self> {
        if data.len() < HEADER_LEN {
            return None;
        }
        let length = be16(data, 4);
        if usize::from(length) < HEADER_LEN || usize::from(length) > data.len() {
            return None;
        }
        let datagram = &data[..usize::from(length)];
        if be16(data, 6) != 0 {
            let sum = checksum_add(pseudo_header_sum(source, destination, length), datagram);
            if checksum_finish(sum) != 0 {
                return None;
            }
        }
        Some(Self {
            source_port: be16(data, 0),
            destination_port: be16(data, 2),
            payload: &datagram[HEADER_LEN..],
        })
    }
}

/// Completes a datagram whose payload of `payload_len` bytes is already
/// at `buffer[HEADER_LEN..]`: writes the header, checksum included, and
/// returns the datagram's length. `None` if the buffer is too small or
/// the datagram too long.
pub fn finish(
    buffer: &mut [u8],
    source: Ipv4Addr,
    source_port: u16,
    destination: Ipv4Addr,
    destination_port: u16,
    payload_len: usize,
) -> Option<usize> {
    let total = HEADER_LEN.checked_add(payload_len)?;
    let length = u16::try_from(total).ok()?;
    let out = buffer.get_mut(..total)?;
    out[0..2].copy_from_slice(&source_port.to_be_bytes());
    out[2..4].copy_from_slice(&destination_port.to_be_bytes());
    out[4..6].copy_from_slice(&length.to_be_bytes());
    out[6..8].fill(0);
    let sum = checksum_finish(checksum_add(
        pseudo_header_sum(source, destination, length),
        out,
    ));
    // A computed checksum of zero is sent as all ones; zero means "none".
    let sum = if sum == 0 { 0xFFFF } else { sum };
    out[6..8].copy_from_slice(&sum.to_be_bytes());
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
    const B: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);

    fn datagram(payload: &[u8]) -> ([u8; 48], usize) {
        let mut buffer = [0u8; 48];
        buffer[8..8 + payload.len()].copy_from_slice(payload);
        let length = finish(&mut buffer, A, 68, B, 67, payload.len()).unwrap();
        (buffer, length)
    }

    #[test]
    fn a_datagram_round_trips_and_padding_is_cut_off() {
        for payload in [&b""[..], b"a", b"hello"] {
            let (buffer, length) = datagram(payload);
            assert_eq!(length, 8 + payload.len());
            let parsed = Datagram::parse(&buffer, A, B).unwrap();
            assert_eq!(parsed.source_port, 68);
            assert_eq!(parsed.destination_port, 67);
            assert_eq!(parsed.payload, payload);
        }
    }

    #[test]
    fn the_bytes_match_a_hand_computed_datagram() {
        let (buffer, length) = datagram(b"hi");
        // Pseudo header: 0a00 020f 0a00 0202 0011 000a = 182c.
        // Header and data: 0044 0043 000a 0000 6869 = 68fa.
        // Sum 8126, checksum 7ed9 (checked with a separate implementation).
        assert_eq!(
            buffer[..length],
            [0, 68, 0, 67, 0, 10, 0x7E, 0xD9, b'h', b'i']
        );
    }

    #[test]
    fn the_checksum_covers_addresses_and_data() {
        let (good, length) = datagram(b"hello");
        let good = &good[..length];
        assert!(Datagram::parse(good, A, B).is_some());
        assert_eq!(Datagram::parse(good, A, Ipv4Addr([10, 0, 2, 3])), None);
        assert_eq!(Datagram::parse(good, Ipv4Addr([10, 0, 3, 15]), B), None);
        for byte in 0..length {
            let mut bad = [0u8; 13];
            bad.copy_from_slice(good);
            bad[byte] ^= 4;
            // Changing the length field to something larger than the
            // data, or the data itself, must both be caught.
            assert_eq!(Datagram::parse(&bad, A, B), None, "byte {byte}");
        }
    }

    #[test]
    fn a_datagram_without_checksum_is_accepted() {
        let (mut buffer, length) = datagram(b"hello");
        buffer[6..8].fill(0);
        buffer[9] ^= 0xFF;
        assert!(Datagram::parse(&buffer[..length], A, B).is_some());
    }

    #[test]
    fn bad_lengths_are_refused() {
        let (buffer, length) = datagram(b"hello");
        assert_eq!(Datagram::parse(&buffer[..length - 1], A, B), None);
        assert_eq!(Datagram::parse(&buffer[..7], A, B), None);
        let mut short = buffer;
        short[4..6].copy_from_slice(&7u16.to_be_bytes());
        short[6..8].fill(0);
        assert_eq!(Datagram::parse(&short, A, B), None);
        assert_eq!(finish(&mut [0; 12], A, 1, B, 2, 5), None);
        assert_eq!(finish(&mut [0; 16], A, 1, B, 2, 70_000), None);
    }
}
