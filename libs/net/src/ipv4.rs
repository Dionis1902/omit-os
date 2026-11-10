// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! IPv4 headers (RFC 791), without options and without fragmentation.

use crate::{Ipv4Addr, be16, checksum, ip_at};

/// Length of a header without options.
pub const HEADER_LEN: usize = 20;

/// Protocol number of ICMP.
pub const PROTOCOL_ICMP: u8 = 1;
/// Protocol number of UDP.
pub const PROTOCOL_UDP: u8 = 17;

/// Time to live given to packets sent from here.
pub const DEFAULT_TTL: u8 = 64;

const FLAG_MORE_FRAGMENTS: u16 = 0x2000;
const FRAGMENT_OFFSET_MASK: u16 = 0x1FFF;
const FLAG_DONT_FRAGMENT: u16 = 0x4000;

/// A received packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet<'a> {
    /// Protocol of the payload.
    pub protocol: u8,
    /// Sender.
    pub source: Ipv4Addr,
    /// Recipient.
    pub destination: Ipv4Addr,
    /// Remaining hops.
    pub ttl: u8,
    /// The payload, exactly as long as the header says (frame padding
    /// removed).
    pub payload: &'a [u8],
}

impl<'a> Packet<'a> {
    /// Parses a packet. `None` if it is not version 4, has options, is a
    /// fragment, fails its header checksum, or is shorter than it claims.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER_LEN || data[0] != 0x45 {
            return None;
        }
        let total = usize::from(be16(data, 2));
        let fragment = be16(data, 6);
        if total < HEADER_LEN
            || total > data.len()
            || fragment & (FLAG_MORE_FRAGMENTS | FRAGMENT_OFFSET_MASK) != 0
            || checksum(&data[..HEADER_LEN]) != 0
        {
            return None;
        }
        Some(Self {
            protocol: data[9],
            source: ip_at(data, 12),
            destination: ip_at(data, 16),
            ttl: data[8],
            payload: &data[HEADER_LEN..total],
        })
    }
}

/// Writes a header for a payload of `payload_len` bytes at the start of
/// `buffer` and returns the header's length. `None` if the buffer cannot
/// hold header and payload, or the packet would exceed 65535 bytes.
pub fn write_header(
    buffer: &mut [u8],
    protocol: u8,
    source: Ipv4Addr,
    destination: Ipv4Addr,
    identification: u16,
    payload_len: usize,
) -> Option<usize> {
    let total = HEADER_LEN.checked_add(payload_len)?;
    if buffer.len() < total {
        return None;
    }
    let total = u16::try_from(total).ok()?;
    let header = &mut buffer[..HEADER_LEN];
    header[0] = 0x45;
    header[1] = 0;
    header[2..4].copy_from_slice(&total.to_be_bytes());
    header[4..6].copy_from_slice(&identification.to_be_bytes());
    header[6..8].copy_from_slice(&FLAG_DONT_FRAGMENT.to_be_bytes());
    header[8] = DEFAULT_TTL;
    header[9] = protocol;
    header[10..12].fill(0);
    header[12..16].copy_from_slice(&source.0);
    header[16..20].copy_from_slice(&destination.0);
    let sum = checksum(header);
    header[10..12].copy_from_slice(&sum.to_be_bytes());
    Some(HEADER_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
    const B: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);

    fn packet(payload: &[u8]) -> ([u8; 64], usize) {
        let mut buffer = [0u8; 64];
        let header = write_header(&mut buffer, PROTOCOL_UDP, A, B, 7, payload.len()).unwrap();
        buffer[header..header + payload.len()].copy_from_slice(payload);
        (buffer, header + payload.len())
    }

    #[test]
    fn a_packet_round_trips_and_padding_is_cut_off() {
        let (buffer, length) = packet(b"hello");
        assert_eq!(length, 25);
        // The whole 64-byte buffer is given: 39 bytes of padding.
        let parsed = Packet::parse(&buffer).unwrap();
        assert_eq!(parsed.protocol, PROTOCOL_UDP);
        assert_eq!(parsed.source, A);
        assert_eq!(parsed.destination, B);
        assert_eq!(parsed.ttl, DEFAULT_TTL);
        assert_eq!(parsed.payload, b"hello");
    }

    #[test]
    fn the_header_matches_a_hand_computed_one() {
        let (buffer, _) = packet(b"hello");
        // 4500 0019 0007 4000 4011 xxxx 0a00020f 0a000202: the words sum
        // to 0xDD42, so the checksum is 0x22BD (checked with a separate
        // implementation).
        assert_eq!(
            buffer[..20],
            [
                0x45, 0, 0, 25, 0, 7, 0x40, 0, 64, 17, 0x22, 0xBD, 10, 0, 2, 15, 10, 0, 2, 2
            ]
        );
    }

    #[test]
    fn damaged_and_unsupported_packets_are_refused() {
        let (good, length) = packet(b"hello");
        assert!(Packet::parse(&good[..length]).is_some());
        // Truncated below the stated length.
        assert_eq!(Packet::parse(&good[..length - 1]), None);
        assert_eq!(Packet::parse(&good[..19]), None);
        // Any single-bit change in the header breaks the checksum.
        for byte in 0..20 {
            let mut bad = good;
            bad[byte] ^= 0x10;
            assert_eq!(Packet::parse(&bad), None, "byte {byte}");
        }
        // Options (header length 6 words) and fragments, with a checksum
        // that would otherwise be right.
        let fix = |mutate: &dyn Fn(&mut [u8; 64])| {
            let mut bad = good;
            mutate(&mut bad);
            bad[10..12].fill(0);
            let sum = checksum(&bad[..20]);
            bad[10..12].copy_from_slice(&sum.to_be_bytes());
            bad
        };
        assert_eq!(Packet::parse(&fix(&|p| p[0] = 0x46)), None);
        assert_eq!(Packet::parse(&fix(&|p| p[0] = 0x65)), None);
        assert_eq!(Packet::parse(&fix(&|p| p[6] = 0x20)), None);
        assert_eq!(Packet::parse(&fix(&|p| p[7] = 1)), None);
        assert_eq!(Packet::parse(&fix(&|p| p[3] = 19)), None);
        // Control: the fixing closure itself keeps a good packet good.
        assert!(Packet::parse(&fix(&|p| p[8] = 3)).is_some());
    }

    #[test]
    fn a_buffer_that_is_too_small_is_refused() {
        assert_eq!(write_header(&mut [0; 24], 1, A, B, 0, 5), None);
        assert_eq!(write_header(&mut [0; 25], 1, A, B, 0, 5), Some(20));
        assert_eq!(write_header(&mut [0; 32], 1, A, B, 0, usize::MAX), None);
    }
}
