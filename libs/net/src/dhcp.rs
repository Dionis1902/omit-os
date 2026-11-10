// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! The client side of DHCP (RFC 2131; options from RFC 2132): the two
//! messages a client sends to obtain an address, and what a server
//! answers.
//!
//! Not handled: options continued in the `sname` and `file` fields
//! (option 52), renewing a lease, releasing one.

use crate::{Ipv4Addr, MacAddr, be32, ip_at};

/// UDP port servers listen on.
pub const SERVER_PORT: u16 = 67;
/// UDP port clients listen on.
pub const CLIENT_PORT: u16 = 68;

/// Length of the fixed part of a message, before the options.
const FIXED_LEN: usize = 236;
/// The four bytes that introduce the options.
const MAGIC: [u8; 4] = [99, 130, 83, 99];
/// Messages are padded to the size every BOOTP relay accepts.
pub const MIN_MESSAGE_LEN: usize = 300;

const OP_REQUEST: u8 = 1;
const OP_REPLY: u8 = 2;
const HARDWARE_ETHERNET: u8 = 1;
/// Asks the server to broadcast its answers: until the client has an
/// address it cannot be reached any other way.
const FLAG_BROADCAST: u16 = 0x8000;

const OPTION_PAD: u8 = 0;
const OPTION_SUBNET_MASK: u8 = 1;
const OPTION_ROUTER: u8 = 3;
const OPTION_DNS: u8 = 6;
const OPTION_REQUESTED_ADDRESS: u8 = 50;
const OPTION_LEASE_TIME: u8 = 51;
const OPTION_MESSAGE_TYPE: u8 = 53;
const OPTION_SERVER: u8 = 54;
const OPTION_PARAMETERS: u8 = 55;
const OPTION_END: u8 = 255;

const TYPE_DISCOVER: u8 = 1;
const TYPE_OFFER: u8 = 2;
const TYPE_REQUEST: u8 = 3;
const TYPE_ACK: u8 = 5;
const TYPE_NAK: u8 = 6;

/// Writes the fixed part and the message type option; returns the offset
/// at which further options go.
fn write_start(buffer: &mut [u8], kind: u8, transaction: u32, mac: MacAddr) -> Option<usize> {
    if buffer.len() < MIN_MESSAGE_LEN {
        return None;
    }
    buffer[..MIN_MESSAGE_LEN].fill(0);
    buffer[0] = OP_REQUEST;
    buffer[1] = HARDWARE_ETHERNET;
    buffer[2] = 6;
    buffer[4..8].copy_from_slice(&transaction.to_be_bytes());
    buffer[10..12].copy_from_slice(&FLAG_BROADCAST.to_be_bytes());
    buffer[28..34].copy_from_slice(&mac.0);
    buffer[FIXED_LEN..FIXED_LEN + 4].copy_from_slice(&MAGIC);
    let at = FIXED_LEN + 4;
    buffer[at..at + 3].copy_from_slice(&[OPTION_MESSAGE_TYPE, 1, kind]);
    Some(at + 3)
}

/// Appends an option and returns the offset after it.
fn write_option(buffer: &mut [u8], at: usize, code: u8, value: &[u8]) -> usize {
    buffer[at] = code;
    buffer[at + 1] = value.len() as u8;
    buffer[at + 2..at + 2 + value.len()].copy_from_slice(value);
    at + 2 + value.len()
}

/// Ends the options with the list of things wanted from the server.
fn write_end(buffer: &mut [u8], at: usize) -> usize {
    let at = write_option(
        buffer,
        at,
        OPTION_PARAMETERS,
        &[OPTION_SUBNET_MASK, OPTION_ROUTER, OPTION_DNS],
    );
    buffer[at] = OPTION_END;
    MIN_MESSAGE_LEN.max(at + 1)
}

/// Writes a DHCPDISCOVER ("is there a server, and what would it give
/// me?") at the start of `buffer`, which must hold [`MIN_MESSAGE_LEN`]
/// bytes; returns the message length.
pub fn write_discover(buffer: &mut [u8], transaction: u32, mac: MacAddr) -> Option<usize> {
    let at = write_start(buffer, TYPE_DISCOVER, transaction, mac)?;
    Some(write_end(buffer, at))
}

/// Writes a DHCPREQUEST accepting `address` as offered by `server`;
/// returns the message length.
pub fn write_request(
    buffer: &mut [u8],
    transaction: u32,
    mac: MacAddr,
    address: Ipv4Addr,
    server: Ipv4Addr,
) -> Option<usize> {
    let at = write_start(buffer, TYPE_REQUEST, transaction, mac)?;
    let at = write_option(buffer, at, OPTION_REQUESTED_ADDRESS, &address.0);
    let at = write_option(buffer, at, OPTION_SERVER, &server.0);
    Some(write_end(buffer, at))
}

/// What kind of answer a server gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    /// "You could have this address."
    Offer,
    /// "The address is yours."
    Ack,
    /// "No."
    Nak,
}

/// A server's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    /// Offer, acknowledgement or refusal.
    pub kind: ReplyKind,
    /// The address offered or assigned.
    pub address: Ipv4Addr,
    /// The server's identifier, to be quoted in the request.
    pub server: Option<Ipv4Addr>,
    /// The network mask.
    pub mask: Option<Ipv4Addr>,
    /// The first router.
    pub router: Option<Ipv4Addr>,
    /// The first name server.
    pub dns: Option<Ipv4Addr>,
    /// How long the address may be used, in seconds.
    pub lease_seconds: Option<u32>,
}

impl Reply {
    /// Parses a server's message. `None` unless it is a well-formed
    /// reply to transaction `transaction` of the client with address
    /// `mac`, of one of the three kinds above.
    pub fn parse(data: &[u8], transaction: u32, mac: MacAddr) -> Option<Self> {
        if data.len() < FIXED_LEN + 4
            || data[0] != OP_REPLY
            || data[1] != HARDWARE_ETHERNET
            || data[2] != 6
            || be32(data, 4) != transaction
            || data[28..34] != mac.0
            || data[FIXED_LEN..FIXED_LEN + 4] != MAGIC
        {
            return None;
        }
        let mut kind = None;
        let mut reply = Self {
            kind: ReplyKind::Nak,
            address: ip_at(data, 16),
            server: None,
            mask: None,
            router: None,
            dns: None,
            lease_seconds: None,
        };
        let mut options = &data[FIXED_LEN + 4..];
        // Options that run out without an end marker are tolerated; an
        // option that claims more bytes than there are is not.
        while let Some((&code, rest)) = options.split_first() {
            if code == OPTION_END {
                break;
            }
            if code == OPTION_PAD {
                options = rest;
                continue;
            }
            let (&length, rest) = rest.split_first()?;
            let length = usize::from(length);
            if rest.len() < length {
                return None;
            }
            let (value, rest) = rest.split_at(length);
            options = rest;
            // Address options may list several; the first is used.
            let address = (value.len() >= 4).then(|| ip_at(value, 0));
            match code {
                OPTION_MESSAGE_TYPE => {
                    kind = Some(match value {
                        [TYPE_OFFER] => ReplyKind::Offer,
                        [TYPE_ACK] => ReplyKind::Ack,
                        [TYPE_NAK] => ReplyKind::Nak,
                        _ => return None,
                    });
                }
                OPTION_SERVER => reply.server = address,
                OPTION_SUBNET_MASK => reply.mask = address,
                OPTION_ROUTER => reply.router = address,
                OPTION_DNS => reply.dns = address,
                OPTION_LEASE_TIME if value.len() == 4 => {
                    reply.lease_seconds = Some(be32(value, 0));
                }
                _ => {}
            }
        }
        reply.kind = kind?;
        Some(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::vec::Vec;

    const MAC: MacAddr = MacAddr([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
    const XID: u32 = 0xDEAD_BEEF;

    /// The options of a client message as (code, value) pairs.
    fn options(message: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut found = Vec::new();
        let mut at = FIXED_LEN + 4;
        while message[at] != OPTION_END {
            let length = usize::from(message[at + 1]);
            found.push((message[at], message[at + 2..at + 2 + length].to_vec()));
            at += 2 + length;
        }
        assert!(message[at + 1..].iter().all(|&b| b == 0), "padding is zero");
        found
    }

    fn check_fixed_part(message: &[u8]) {
        assert_eq!(message.len(), 300);
        assert_eq!(message[..4], [1, 1, 6, 0]);
        assert_eq!(message[4..8], [0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(
            message[8..12],
            [0, 0, 0x80, 0],
            "no seconds, broadcast flag"
        );
        assert!(message[12..28].iter().all(|&b| b == 0), "no addresses yet");
        assert_eq!(message[28..34], MAC.0);
        assert!(message[34..236].iter().all(|&b| b == 0));
        assert_eq!(message[236..240], [99, 130, 83, 99]);
    }

    #[test]
    fn discover_has_the_fields_the_rfc_requires() {
        let mut buffer = [0xEEu8; 400];
        let length = write_discover(&mut buffer, XID, MAC).unwrap();
        let message = &buffer[..length];
        check_fixed_part(message);
        assert_eq!(
            options(message),
            [(53, [1].to_vec()), (55, [1, 3, 6].to_vec())]
        );
        assert_eq!(buffer[300], 0xEE, "nothing written past the message");
    }

    #[test]
    fn request_names_the_address_and_the_server() {
        let mut buffer = [0xEEu8; 300];
        let length = write_request(
            &mut buffer,
            XID,
            MAC,
            Ipv4Addr([10, 0, 2, 15]),
            Ipv4Addr([10, 0, 2, 2]),
        )
        .unwrap();
        let message = &buffer[..length];
        check_fixed_part(message);
        assert_eq!(
            options(message),
            [
                (53, [3].to_vec()),
                (50, [10, 0, 2, 15].to_vec()),
                (54, [10, 0, 2, 2].to_vec()),
                (55, [1, 3, 6].to_vec()),
            ]
        );
    }

    #[test]
    fn a_buffer_that_is_too_small_is_refused() {
        assert_eq!(write_discover(&mut [0; 299], XID, MAC), None);
        assert_eq!(
            write_request(
                &mut [0; 299],
                XID,
                MAC,
                Ipv4Addr::UNSPECIFIED,
                Ipv4Addr::UNSPECIFIED
            ),
            None
        );
    }

    /// A server reply with the given options.
    fn reply(options: &[u8]) -> Vec<u8> {
        let mut message = std::vec![0u8; FIXED_LEN];
        message[0] = 2;
        message[1] = 1;
        message[2] = 6;
        message[4..8].copy_from_slice(&XID.to_be_bytes());
        message[16..20].copy_from_slice(&[10, 0, 2, 15]);
        message[28..34].copy_from_slice(&MAC.0);
        message.extend_from_slice(&MAGIC);
        message.extend_from_slice(options);
        message
    }

    const OFFER_OPTIONS: &[u8] = &[
        53, 1, 2, // offer
        54, 4, 10, 0, 2, 2, // server
        1, 4, 255, 255, 255, 0, // mask
        0, 0, // padding between options
        3, 8, 10, 0, 2, 2, 10, 0, 2, 9, // two routers
        6, 4, 10, 0, 2, 3, // name server
        51, 4, 0, 1, 0x51, 0x80, // one day
        15, 3, b'l', b'a', b'n', // an option not asked for
        255,
    ];

    #[test]
    fn an_offer_is_understood() {
        let parsed = Reply::parse(&reply(OFFER_OPTIONS), XID, MAC).unwrap();
        assert_eq!(
            parsed,
            Reply {
                kind: ReplyKind::Offer,
                address: Ipv4Addr([10, 0, 2, 15]),
                server: Some(Ipv4Addr([10, 0, 2, 2])),
                mask: Some(Ipv4Addr([255, 255, 255, 0])),
                router: Some(Ipv4Addr([10, 0, 2, 2])),
                dns: Some(Ipv4Addr([10, 0, 2, 3])),
                lease_seconds: Some(86_400),
            }
        );
    }

    #[test]
    fn acknowledgement_and_refusal_are_told_apart() {
        let ack = Reply::parse(&reply(&[53, 1, 5, 255]), XID, MAC).unwrap();
        assert_eq!(ack.kind, ReplyKind::Ack);
        assert_eq!(ack.mask, None);
        let nak = Reply::parse(&reply(&[53, 1, 6, 255]), XID, MAC).unwrap();
        assert_eq!(nak.kind, ReplyKind::Nak);
        // Options may simply stop.
        assert!(Reply::parse(&reply(&[53, 1, 5]), XID, MAC).is_some());
    }

    #[test]
    fn replies_meant_for_someone_else_are_ignored() {
        let good = reply(OFFER_OPTIONS);
        assert_eq!(Reply::parse(&good, XID + 1, MAC), None);
        assert_eq!(Reply::parse(&good, XID, MacAddr([2, 0, 0, 0, 0, 1])), None);
        // A client's message is not a reply.
        let mut request = good.clone();
        request[0] = 1;
        assert_eq!(Reply::parse(&request, XID, MAC), None);
    }

    #[test]
    fn malformed_replies_are_refused() {
        let good = reply(OFFER_OPTIONS);
        assert_eq!(Reply::parse(&good[..239], XID, MAC), None);
        let mut cookie = good.clone();
        cookie[237] = 0;
        assert_eq!(Reply::parse(&cookie, XID, MAC), None);
        // No message type; a type that is not a server's; a type of the
        // wrong length.
        assert_eq!(
            Reply::parse(&reply(&[1, 4, 255, 0, 0, 0, 255]), XID, MAC),
            None
        );
        assert_eq!(Reply::parse(&reply(&[53, 1, 1, 255]), XID, MAC), None);
        assert_eq!(Reply::parse(&reply(&[53, 2, 2, 2, 255]), XID, MAC), None);
        // An option longer than the message; a code without a length.
        assert_eq!(
            Reply::parse(&reply(&[53, 1, 2, 1, 4, 255, 255]), XID, MAC),
            None
        );
        assert_eq!(Reply::parse(&reply(&[53, 1, 2, 1]), XID, MAC), None);
        // Every truncation of a good reply is either refused or parsed,
        // never a panic.
        for length in 0..good.len() {
            let _ = Reply::parse(&good[..length], XID, MAC);
        }
    }

    #[test]
    fn short_address_options_are_ignored() {
        let parsed = Reply::parse(&reply(&[53, 1, 5, 3, 2, 10, 0, 51, 2, 0, 1, 255]), XID, MAC);
        let parsed = parsed.unwrap();
        assert_eq!(parsed.router, None);
        assert_eq!(parsed.lease_seconds, None);
    }
}
