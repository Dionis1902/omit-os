// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Network protocol formats, written from the protocol specifications:
//! Ethernet II framing, ARP (RFC 826), IPv4 (RFC 791), ICMP echo
//! (RFC 792), UDP (RFC 768), the Internet checksum (RFC 1071) and the
//! client side of DHCP (RFC 2131, options from RFC 2132).
//!
//! Everything here is a pure function over byte slices: parsing borrows
//! from the packet, building writes into a caller-supplied buffer and
//! returns the length written. No allocation, no I/O, no state — the
//! kernel's network module supplies those, and the code is tested on the
//! build machine.
//!
//! Deliberately absent: IPv4 options and fragments (packets carrying
//! either are rejected), IPv6, TCP.

#![no_std]
#![forbid(unsafe_code)]

pub mod arp;
pub mod dhcp;
pub mod ethernet;
pub mod icmp;
pub mod ipv4;
pub mod stack;
pub mod udp;

use core::fmt;

/// An Ethernet hardware address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// The address every station receives.
    pub const BROADCAST: Self = Self([0xFF; 6]);
    /// All zeroes: "unknown" in ARP requests.
    pub const ZERO: Self = Self([0; 6]);
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

/// An IPv4 address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    /// `0.0.0.0`: "this host", before it has an address.
    pub const UNSPECIFIED: Self = Self([0; 4]);
    /// `255.255.255.255`: everyone on the local network.
    pub const BROADCAST: Self = Self([255; 4]);

    /// Parses dotted-decimal notation (`10.0.2.2`): exactly four decimal
    /// numbers of one to three digits, each at most 255.
    pub fn parse(text: &str) -> Option<Self> {
        let mut octets = [0u8; 4];
        let mut parts = text.split('.');
        for octet in &mut octets {
            let part = parts.next()?;
            if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *octet = part.parse().ok()?;
        }
        parts.next().is_none().then_some(Self(octets))
    }

    /// The address as a number, most significant octet first.
    pub fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }

    /// Whether `self` and `other` are on the same network under `mask`.
    pub fn same_network(self, other: Self, mask: Self) -> bool {
        (self.to_u32() ^ other.to_u32()) & mask.to_u32() == 0
    }

    /// The number of leading one bits of a network mask, or `None` if the
    /// ones are not contiguous.
    pub fn prefix_length(self) -> Option<u32> {
        let mask = self.to_u32();
        let ones = mask.leading_ones();
        (mask.checked_shl(ones).unwrap_or(0) == 0).then_some(ones)
    }
}

impl fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a}.{b}.{c}.{d}")
    }
}

/// The running sum of the Internet checksum (RFC 1071): 16-bit big-endian
/// words added with the carries folded back in. Sums of several pieces
/// can be chained through `initial`, provided every piece but the last
/// has an even length.
pub fn checksum_add(initial: u32, data: &[u8]) -> u32 {
    let mut sum = initial;
    let mut words = data.chunks_exact(2);
    for word in &mut words {
        sum += u32::from(u16::from_be_bytes([word[0], word[1]]));
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    if let [last] = words.remainder() {
        sum += u32::from(*last) << 8;
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    sum
}

/// Finishes a checksum: the one's complement of the folded sum.
pub fn checksum_finish(sum: u32) -> u16 {
    let folded = (sum & 0xFFFF) + (sum >> 16);
    !((folded & 0xFFFF) + (folded >> 16)) as u16
}

/// The Internet checksum of `data`. Data that includes a correct checksum
/// field sums to zero.
pub fn checksum(data: &[u8]) -> u16 {
    checksum_finish(checksum_add(0, data))
}

/// Reads a big-endian `u16` at `offset`; the caller has checked the length.
pub(crate) fn be16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

/// Reads a big-endian `u32` at `offset`; the caller has checked the length.
pub(crate) fn be32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Reads four address octets at `offset`; the caller has checked the length.
pub(crate) fn ip_at(data: &[u8], offset: usize) -> Ipv4Addr {
    Ipv4Addr([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Reads six address octets at `offset`; the caller has checked the length.
pub(crate) fn mac_at(data: &[u8], offset: usize) -> MacAddr {
    let mut mac = [0; 6];
    mac.copy_from_slice(&data[offset..offset + 6]);
    MacAddr(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::format;

    #[test]
    fn addresses_are_displayed_in_the_usual_notation() {
        assert_eq!(format!("{}", Ipv4Addr([10, 0, 2, 15])), "10.0.2.15");
        assert_eq!(
            format!("{}", MacAddr([0x52, 0x54, 0, 0x12, 0x34, 0x56])),
            "52:54:00:12:34:56"
        );
    }

    #[test]
    fn dotted_decimal_is_parsed_strictly() {
        assert_eq!(Ipv4Addr::parse("10.0.2.2"), Some(Ipv4Addr([10, 0, 2, 2])));
        assert_eq!(
            Ipv4Addr::parse("255.255.255.255"),
            Some(Ipv4Addr::BROADCAST)
        );
        assert_eq!(Ipv4Addr::parse("0.0.0.0"), Some(Ipv4Addr::UNSPECIFIED));
        for bad in [
            "",
            "1.2.3",
            "1.2.3.4.5",
            "1.2.3.256",
            "1..2.3",
            "a.b.c.d",
            "1.2.3.4 ",
            "+1.2.3.4",
            "1.2.3.0004",
            "1.2.3.-4",
        ] {
            assert_eq!(Ipv4Addr::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn networks_and_masks() {
        let mask = Ipv4Addr([255, 255, 255, 0]);
        let host = Ipv4Addr([10, 0, 2, 15]);
        assert!(host.same_network(Ipv4Addr([10, 0, 2, 2]), mask));
        assert!(!host.same_network(Ipv4Addr([10, 0, 3, 2]), mask));
        assert!(host.same_network(Ipv4Addr([8, 8, 8, 8]), Ipv4Addr::UNSPECIFIED));
        assert_eq!(mask.prefix_length(), Some(24));
        assert_eq!(Ipv4Addr::UNSPECIFIED.prefix_length(), Some(0));
        assert_eq!(Ipv4Addr::BROADCAST.prefix_length(), Some(32));
        assert_eq!(Ipv4Addr([255, 0, 255, 0]).prefix_length(), None);
    }

    #[test]
    fn checksum_matches_the_worked_example_of_rfc_1071() {
        // RFC 1071, section 3: the words 0001 f203 f4f5 f6f7 sum to ddf2.
        let data = [0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7];
        assert_eq!(checksum(&data), !0xDDF2);
    }

    #[test]
    fn checksum_handles_odd_lengths_and_chaining() {
        // An odd final byte counts as the high half of a word.
        assert_eq!(checksum(&[0x12]), !0x1200);
        let data = [1u8, 2, 3, 4, 5, 6, 7];
        let chained = checksum_finish(checksum_add(checksum_add(0, &data[..4]), &data[4..]));
        assert_eq!(chained, checksum(&data));
        // Many carries.
        assert_eq!(checksum(&[0xFF; 64]), 0);
    }

    #[test]
    fn data_with_its_checksum_in_place_sums_to_zero() {
        let mut data = [
            0x45u8, 0x00, 0x00, 0x1C, 0xAB, 0xCD, 0, 0, 64, 1, 0, 0, 10, 0, 2, 15, 10, 0, 2, 2,
        ];
        let sum = checksum(&data);
        data[10..12].copy_from_slice(&sum.to_be_bytes());
        assert_eq!(checksum(&data), 0);
    }
}
