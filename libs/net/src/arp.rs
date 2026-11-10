// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! ARP for IPv4 over Ethernet (RFC 826): who has this address, and the
//! answer.

use crate::{Ipv4Addr, MacAddr, be16, ip_at, mac_at};

/// Length of an ARP packet for Ethernet and IPv4.
pub const PACKET_LEN: usize = 28;

const HARDWARE_ETHERNET: u16 = 1;
const PROTOCOL_IPV4: u16 = 0x0800;

/// What an ARP packet asks or says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// "Who has `target_ip`? Tell `sender_ip`."
    Request,
    /// "`sender_ip` is at `sender_mac`."
    Reply,
}

/// An ARP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    /// Question or answer.
    pub operation: Operation,
    /// Hardware address of the sender.
    pub sender_mac: MacAddr,
    /// Protocol address of the sender.
    pub sender_ip: Ipv4Addr,
    /// Hardware address of the target; zero in a request.
    pub target_mac: MacAddr,
    /// Protocol address of the target.
    pub target_ip: Ipv4Addr,
}

impl Packet {
    /// Parses an ARP packet; `None` unless it is a request or reply for
    /// IPv4 over Ethernet. Trailing bytes (frame padding) are ignored.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < PACKET_LEN
            || be16(data, 0) != HARDWARE_ETHERNET
            || be16(data, 2) != PROTOCOL_IPV4
            || data[4] != 6
            || data[5] != 4
        {
            return None;
        }
        let operation = match be16(data, 6) {
            1 => Operation::Request,
            2 => Operation::Reply,
            _ => return None,
        };
        Some(Self {
            operation,
            sender_mac: mac_at(data, 8),
            sender_ip: ip_at(data, 14),
            target_mac: mac_at(data, 18),
            target_ip: ip_at(data, 24),
        })
    }

    /// Writes the packet at the start of `buffer` and returns its length,
    /// or `None` if the buffer is too small.
    pub fn write(&self, buffer: &mut [u8]) -> Option<usize> {
        let out = buffer.get_mut(..PACKET_LEN)?;
        out[0..2].copy_from_slice(&HARDWARE_ETHERNET.to_be_bytes());
        out[2..4].copy_from_slice(&PROTOCOL_IPV4.to_be_bytes());
        out[4] = 6;
        out[5] = 4;
        let operation: u16 = match self.operation {
            Operation::Request => 1,
            Operation::Reply => 2,
        };
        out[6..8].copy_from_slice(&operation.to_be_bytes());
        out[8..14].copy_from_slice(&self.sender_mac.0);
        out[14..18].copy_from_slice(&self.sender_ip.0);
        out[18..24].copy_from_slice(&self.target_mac.0);
        out[24..28].copy_from_slice(&self.target_ip.0);
        Some(PACKET_LEN)
    }

    /// The reply to this request, from the station that owns the address
    /// asked for.
    pub fn reply_from(&self, mac: MacAddr) -> Self {
        Self {
            operation: Operation::Reply,
            sender_mac: mac,
            sender_ip: self.target_ip,
            target_mac: self.sender_mac,
            target_ip: self.sender_ip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: [u8; 28] = [
        0, 1, 8, 0, 6, 4, 0, 1, // Ethernet, IPv4, 6, 4, request
        0x52, 0x54, 0, 0x12, 0x34, 0x56, 10, 0, 2, 15, // sender
        0, 0, 0, 0, 0, 0, 10, 0, 2, 2, // target
    ];

    #[test]
    fn a_request_is_parsed_and_written_back_identically() {
        let packet = Packet::parse(&REQUEST).unwrap();
        assert_eq!(packet.operation, Operation::Request);
        assert_eq!(
            packet.sender_mac,
            MacAddr([0x52, 0x54, 0, 0x12, 0x34, 0x56])
        );
        assert_eq!(packet.sender_ip, Ipv4Addr([10, 0, 2, 15]));
        assert_eq!(packet.target_mac, MacAddr::ZERO);
        assert_eq!(packet.target_ip, Ipv4Addr([10, 0, 2, 2]));
        let mut buffer = [0xFFu8; 28];
        assert_eq!(packet.write(&mut buffer), Some(28));
        assert_eq!(buffer, REQUEST);
    }

    #[test]
    fn the_reply_swaps_the_parties() {
        let mac = MacAddr([2, 0, 0, 0, 0, 9]);
        let reply = Packet::parse(&REQUEST).unwrap().reply_from(mac);
        assert_eq!(reply.operation, Operation::Reply);
        assert_eq!(reply.sender_mac, mac);
        assert_eq!(reply.sender_ip, Ipv4Addr([10, 0, 2, 2]));
        assert_eq!(reply.target_ip, Ipv4Addr([10, 0, 2, 15]));
        let mut buffer = [0u8; 28];
        reply.write(&mut buffer).unwrap();
        assert_eq!(Packet::parse(&buffer), Some(reply));
    }

    #[test]
    fn padding_is_ignored_and_other_packets_are_refused() {
        let mut padded = [0u8; 46];
        padded[..28].copy_from_slice(&REQUEST);
        assert!(Packet::parse(&padded).is_some());
        assert_eq!(Packet::parse(&REQUEST[..27]), None);
        for (offset, value) in [(1, 6), (3, 6), (4, 8), (5, 16), (7, 3)] {
            let mut bad = REQUEST;
            bad[offset] = value;
            assert_eq!(Packet::parse(&bad), None, "byte {offset}");
        }
        assert_eq!(Packet::parse(&REQUEST).unwrap().write(&mut [0; 27]), None);
    }
}
