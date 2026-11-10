// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! The state of one network interface and what it does with each frame:
//! an ARP cache, a DHCP client, answers to echo requests, the sending
//! side of `ping`, UDP datagrams to and from local ports, and the
//! passing of TCP segments to and from whoever keeps the connections.
//!
//! The stack does no I/O and reads no clock: frames come in through
//! [`Stack::receive`], go out through a [`Link`], and time is whatever
//! the caller passes to [`Stack::tick`]. That is what lets the tests
//! below play the part of the rest of the network.
//!
//! Simplifications: one interface, one address; an unanswered ARP
//! request is repeated at most every [`ARP_RETRY_MS`], however often
//! the address is asked for; ARP entries never expire
//! (they are overwritten when the owner announces a change, and the
//! oldest is replaced when the table is full); a DHCP lease is recorded
//! but never renewed.

use crate::arp::{self, Operation};
use crate::dhcp::{self, Reply, ReplyKind};
use crate::ethernet::{self, Frame, MAX_FRAME_LEN};
use crate::icmp::{Echo, Kind};
use crate::ipv4::{self, PROTOCOL_ICMP, PROTOCOL_TCP, PROTOCOL_UDP};
use crate::udp::{self, Datagram};
use crate::{Ipv4Addr, MacAddr};

/// Where the stack sends frames.
pub trait Link {
    /// Sends one Ethernet frame (without check sequence). Failures are
    /// the link's to count; to the protocols a frame that was not sent is
    /// a frame that was lost.
    fn send(&mut self, frame: &[u8]);
}

/// Where what arrives for the transport protocols goes.
pub trait Sockets {
    /// A UDP datagram has arrived for local port `port`. Returns whether
    /// anybody is listening there; if not, the datagram is dropped.
    fn udp(&mut self, from: Ipv4Addr, from_port: u16, port: u16, data: &[u8]) -> bool;

    /// A TCP segment (still unparsed: see [`crate::tcp::Segment`]) has
    /// arrived from `from` for this interface's address `to`. Returns
    /// whether it belonged to a connection or opened one.
    fn tcp(&mut self, from: Ipv4Addr, to: Ipv4Addr, segment: &[u8]) -> bool;
}

/// For interfaces on which nobody listens to anything.
pub struct NoSockets;

impl Sockets for NoSockets {
    fn udp(&mut self, _: Ipv4Addr, _: u16, _: u16, _: &[u8]) -> bool {
        false
    }

    fn tcp(&mut self, _: Ipv4Addr, _: Ipv4Addr, _: &[u8]) -> bool {
        false
    }
}

/// Longest UDP payload that fits one frame.
pub const MAX_UDP_PAYLOAD: usize = MAX_IP_PAYLOAD - udp::HEADER_LEN;

/// The addresses an interface works with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The interface's own address.
    pub address: Ipv4Addr,
    /// Which part of an address names the local network.
    pub mask: Ipv4Addr,
    /// Where packets for other networks go.
    pub router: Option<Ipv4Addr>,
    /// A name server, recorded for later use.
    pub dns: Option<Ipv4Addr>,
    /// How long the DHCP server lent the address for, in seconds.
    pub lease_seconds: Option<u32>,
}

/// How far the DHCP client has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpState {
    /// Not started.
    Off,
    /// Asking whether there is a server.
    Discovering,
    /// Asking the server that answered for the address it offered.
    Requesting,
    /// An address has been assigned.
    Bound,
    /// No server answered.
    Failed,
}

/// Why something could not be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The interface has no address yet.
    NotConfigured,
    /// The destination is on another network and no router is known.
    NoRoute,
    /// The hardware address of the next hop is not known yet; an ARP
    /// request has been sent, so trying again shortly may succeed.
    Unresolved,
    /// The data does not fit in one frame.
    TooLong,
}

/// An echo reply that has arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoReply {
    /// Who answered.
    pub from: Ipv4Addr,
    /// Identifier of the request it answers.
    pub identifier: u16,
    /// Sequence number of the request it answers.
    pub sequence: u16,
    /// Hops the reply had left when it arrived.
    pub ttl: u8,
    /// Bytes of data returned.
    pub length: usize,
}

/// Counters, for `ifconfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Frames handed to [`Stack::receive`].
    pub received: u64,
    /// Of those, frames that were not understood or not for us.
    pub ignored: u64,
    /// Frames sent.
    pub sent: u64,
}

const ARP_ENTRIES: usize = 16;
const ECHO_REPLIES: usize = 8;
/// Longest ICMP or UDP payload that fits a frame.
const MAX_IP_PAYLOAD: usize = MAX_FRAME_LEN - ethernet::HEADER_LEN - ipv4::HEADER_LEN;

/// Shortest time between two ARP requests for the same address.
pub const ARP_RETRY_MS: u64 = 200;
/// Milliseconds between DHCP retransmissions.
pub const DHCP_RETRY_MS: u64 = 1000;
/// Messages sent in one state before giving up on it.
pub const DHCP_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Copy)]
struct Lease {
    address: Ipv4Addr,
    server: Ipv4Addr,
}

/// One network interface.
pub struct Stack {
    mac: MacAddr,
    config: Option<Config>,
    arp: [Option<(Ipv4Addr, MacAddr)>; ARP_ENTRIES],
    /// Entry to replace next when the table is full.
    arp_next: usize,
    /// The address last asked for by ARP, and when.
    arp_asked: Option<(Ipv4Addr, u64)>,
    /// The time last passed in by the caller.
    now_ms: u64,
    dhcp: DhcpState,
    transaction: u32,
    /// The offer being requested.
    lease: Option<Lease>,
    attempts: u32,
    /// When the next DHCP message is due.
    next_at_ms: u64,
    replies: [Option<EchoReply>; ECHO_REPLIES],
    replies_next: usize,
    identification: u16,
    stats: Stats,
    /// The frame being built.
    frame: [u8; MAX_FRAME_LEN],
}

impl Stack {
    /// An interface with hardware address `mac` and no IP address.
    /// `seed` varies the DHCP transaction number between machines and
    /// boots.
    pub fn new(mac: MacAddr, seed: u32) -> Self {
        Self {
            mac,
            config: None,
            arp: [None; ARP_ENTRIES],
            arp_next: 0,
            arp_asked: None,
            now_ms: 0,
            dhcp: DhcpState::Off,
            transaction: seed,
            lease: None,
            attempts: 0,
            next_at_ms: 0,
            replies: [None; ECHO_REPLIES],
            replies_next: 0,
            identification: seed as u16,
            stats: Stats::default(),
            frame: [0; MAX_FRAME_LEN],
        }
    }

    /// The interface's hardware address.
    pub fn mac(&self) -> MacAddr {
        self.mac
    }

    /// The interface's addresses, once it has them.
    pub fn config(&self) -> Option<Config> {
        self.config
    }

    /// Sets the addresses by hand, ending any DHCP exchange.
    pub fn set_config(&mut self, config: Config) {
        self.config = Some(config);
        self.dhcp = DhcpState::Off;
    }

    /// How far the DHCP client has got.
    pub fn dhcp_state(&self) -> DhcpState {
        self.dhcp
    }

    /// The counters.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The known neighbours, in no particular order.
    pub fn neighbours(&self) -> impl Iterator<Item = (Ipv4Addr, MacAddr)> + '_ {
        self.arp.iter().flatten().copied()
    }

    /// Starts (or restarts) asking for an address by DHCP; the first
    /// message goes out at the next [`Stack::tick`].
    pub fn start_dhcp(&mut self) {
        self.config = None;
        self.begin_discovery(0);
    }

    fn begin_discovery(&mut self, at_ms: u64) {
        self.dhcp = DhcpState::Discovering;
        self.transaction = self
            .transaction
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        self.lease = None;
        self.attempts = 0;
        self.next_at_ms = at_ms;
    }

    /// Does what is due at time `now_ms` (any clock that only moves
    /// forward): sends and repeats DHCP messages.
    pub fn tick(&mut self, now_ms: u64, link: &mut dyn Link) {
        self.now_ms = now_ms;
        if !matches!(self.dhcp, DhcpState::Discovering | DhcpState::Requesting)
            || now_ms < self.next_at_ms
        {
            return;
        }
        if self.attempts == DHCP_ATTEMPTS {
            if self.dhcp == DhcpState::Requesting {
                // The server that made the offer has gone quiet.
                self.begin_discovery(now_ms);
            } else {
                self.dhcp = DhcpState::Failed;
                return;
            }
        }
        self.attempts += 1;
        self.next_at_ms = now_ms + DHCP_RETRY_MS;
        self.send_dhcp(link);
    }

    /// Sends the DHCP message of the current state.
    fn send_dhcp(&mut self, link: &mut dyn Link) {
        let at = ethernet::HEADER_LEN + ipv4::HEADER_LEN + udp::HEADER_LEN;
        let message = &mut self.frame[at..];
        let length = match (self.dhcp, self.lease) {
            (DhcpState::Requesting, Some(lease)) => dhcp::write_request(
                message,
                self.transaction,
                self.mac,
                lease.address,
                lease.server,
            ),
            _ => dhcp::write_discover(message, self.transaction, self.mac),
        };
        let Some(length) = length else { return };
        let Some(length) = udp::finish(
            &mut self.frame[ethernet::HEADER_LEN + ipv4::HEADER_LEN..],
            Ipv4Addr::UNSPECIFIED,
            dhcp::CLIENT_PORT,
            Ipv4Addr::BROADCAST,
            dhcp::SERVER_PORT,
            length,
        ) else {
            return;
        };
        self.send_ip(
            MacAddr::BROADCAST,
            PROTOCOL_UDP,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            length,
            link,
        );
    }

    /// Sends the IP payload of `length` bytes that is already in place in
    /// the frame buffer.
    fn send_ip(
        &mut self,
        to: MacAddr,
        protocol: u8,
        source: Ipv4Addr,
        destination: Ipv4Addr,
        length: usize,
        link: &mut dyn Link,
    ) {
        self.identification = self.identification.wrapping_add(1);
        let (header, packet) = self.frame.split_at_mut(ethernet::HEADER_LEN);
        if ethernet::write_header(header, to, self.mac, ethernet::TYPE_IPV4).is_none()
            || ipv4::write_header(
                packet,
                protocol,
                source,
                destination,
                self.identification,
                length,
            )
            .is_none()
        {
            return;
        }
        let total = ethernet::HEADER_LEN + ipv4::HEADER_LEN + length;
        let total = ethernet::pad(&mut self.frame, total);
        self.stats.sent += 1;
        link.send(&self.frame[..total]);
    }

    fn send_arp(&mut self, to: MacAddr, packet: arp::Packet, link: &mut dyn Link) {
        let (header, body) = self.frame.split_at_mut(ethernet::HEADER_LEN);
        if ethernet::write_header(header, to, self.mac, ethernet::TYPE_ARP).is_none()
            || packet.write(body).is_none()
        {
            return;
        }
        let total = ethernet::pad(&mut self.frame, ethernet::HEADER_LEN + arp::PACKET_LEN);
        self.stats.sent += 1;
        link.send(&self.frame[..total]);
    }

    fn lookup(&self, address: Ipv4Addr) -> Option<MacAddr> {
        self.neighbours()
            .find(|&(known, _)| known == address)
            .map(|(_, mac)| mac)
    }

    /// Records that `address` is at `mac`. An existing entry is always
    /// brought up to date; a new one is added only if `add` is set.
    fn learn(&mut self, address: Ipv4Addr, mac: MacAddr, add: bool) {
        if address == Ipv4Addr::UNSPECIFIED {
            return;
        }
        if let Some(entry) = self
            .arp
            .iter_mut()
            .flatten()
            .find(|(known, _)| *known == address)
        {
            entry.1 = mac;
        } else if add {
            let slot = self
                .arp
                .iter()
                .position(Option::is_none)
                .unwrap_or_else(|| {
                    self.arp_next = (self.arp_next + 1) % ARP_ENTRIES;
                    self.arp_next
                });
            self.arp[slot] = Some((address, mac));
        }
    }

    /// The hardware address packets for `destination` go to: its own if
    /// it is on the local network, the router's otherwise. If that
    /// address is not known yet an ARP request is sent.
    pub fn resolve(
        &mut self,
        destination: Ipv4Addr,
        link: &mut dyn Link,
    ) -> Result<MacAddr, SendError> {
        let config = self.config.ok_or(SendError::NotConfigured)?;
        // The limited broadcast address, or the local network's own (all
        // host bits set), where the network has host bits at all.
        if destination == Ipv4Addr::BROADCAST
            || (config.mask != Ipv4Addr::BROADCAST
                && destination.same_network(config.address, config.mask)
                && destination.to_u32() | config.mask.to_u32() == u32::MAX)
        {
            return Ok(MacAddr::BROADCAST);
        }
        let next_hop = if destination.same_network(config.address, config.mask) {
            destination
        } else {
            config.router.ok_or(SendError::NoRoute)?
        };
        if let Some(mac) = self.lookup(next_hop) {
            return Ok(mac);
        }
        // Asked a moment ago: the answer may still be on its way.
        if self.arp_asked.is_some_and(|(address, at)| {
            address == next_hop && self.now_ms.saturating_sub(at) < ARP_RETRY_MS
        }) {
            return Err(SendError::Unresolved);
        }
        self.arp_asked = Some((next_hop, self.now_ms));
        let request = arp::Packet {
            operation: Operation::Request,
            sender_mac: self.mac,
            sender_ip: config.address,
            target_mac: MacAddr::ZERO,
            target_ip: next_hop,
        };
        self.send_arp(MacAddr::BROADCAST, request, link);
        Err(SendError::Unresolved)
    }

    /// Sends an echo request to `destination`.
    pub fn send_echo(
        &mut self,
        destination: Ipv4Addr,
        identifier: u16,
        sequence: u16,
        payload: &[u8],
        link: &mut dyn Link,
    ) -> Result<(), SendError> {
        let config = self.config.ok_or(SendError::NotConfigured)?;
        if payload.len() > MAX_IP_PAYLOAD - crate::icmp::HEADER_LEN {
            return Err(SendError::TooLong);
        }
        let to = self.resolve(destination, link)?;
        let echo = Echo {
            kind: Kind::Request,
            identifier,
            sequence,
            payload,
        };
        let length = echo
            .write(&mut self.frame[ethernet::HEADER_LEN + ipv4::HEADER_LEN..])
            .ok_or(SendError::TooLong)?;
        self.send_ip(to, PROTOCOL_ICMP, config.address, destination, length, link);
        Ok(())
    }

    /// Sends a UDP datagram from local port `source_port`.
    pub fn send_udp(
        &mut self,
        destination: Ipv4Addr,
        source_port: u16,
        destination_port: u16,
        payload: &[u8],
        link: &mut dyn Link,
    ) -> Result<(), SendError> {
        let config = self.config.ok_or(SendError::NotConfigured)?;
        if payload.len() > MAX_UDP_PAYLOAD {
            return Err(SendError::TooLong);
        }
        let to = self.resolve(destination, link)?;
        let datagram = &mut self.frame[ethernet::HEADER_LEN + ipv4::HEADER_LEN..];
        datagram[udp::HEADER_LEN..udp::HEADER_LEN + payload.len()].copy_from_slice(payload);
        let length = udp::finish(
            datagram,
            config.address,
            source_port,
            destination,
            destination_port,
            payload.len(),
        )
        .ok_or(SendError::TooLong)?;
        self.send_ip(to, PROTOCOL_UDP, config.address, destination, length, link);
        Ok(())
    }

    /// Sends `segment`, a complete TCP segment whose checksum was
    /// computed for this interface's address and `destination`.
    pub fn send_tcp(
        &mut self,
        destination: Ipv4Addr,
        segment: &[u8],
        link: &mut dyn Link,
    ) -> Result<(), SendError> {
        let config = self.config.ok_or(SendError::NotConfigured)?;
        if segment.len() > MAX_IP_PAYLOAD {
            return Err(SendError::TooLong);
        }
        let to = self.resolve(destination, link)?;
        let at = ethernet::HEADER_LEN + ipv4::HEADER_LEN;
        self.frame[at..at + segment.len()].copy_from_slice(segment);
        self.send_ip(
            to,
            PROTOCOL_TCP,
            config.address,
            destination,
            segment.len(),
            link,
        );
        Ok(())
    }

    /// Whether `address` is one this interface receives for: its own,
    /// or a broadcast address of its network.
    fn is_ours(&self, address: Ipv4Addr) -> bool {
        self.config.is_some_and(|config| {
            address == config.address
                || address == Ipv4Addr::BROADCAST
                || (config.mask != Ipv4Addr::BROADCAST
                    && address.same_network(config.address, config.mask)
                    && address.to_u32() | config.mask.to_u32() == u32::MAX)
        })
    }

    /// Removes and returns the reply from `from` to the echo request
    /// with this identifier and sequence number, if it has arrived.
    pub fn take_echo_reply(
        &mut self,
        from: Ipv4Addr,
        identifier: u16,
        sequence: u16,
    ) -> Option<EchoReply> {
        self.replies
            .iter_mut()
            .find(|slot| {
                slot.is_some_and(|reply| {
                    reply.from == from
                        && reply.identifier == identifier
                        && reply.sequence == sequence
                })
            })?
            .take()
    }

    /// Takes in one received frame, at time `now_ms`.
    pub fn receive(
        &mut self,
        data: &[u8],
        now_ms: u64,
        link: &mut dyn Link,
        sockets: &mut dyn Sockets,
    ) {
        self.now_ms = now_ms;
        self.stats.received += 1;
        if !self.handle(data, now_ms, link, sockets) {
            self.stats.ignored += 1;
        }
    }

    /// Returns whether the frame was for us and understood.
    fn handle(
        &mut self,
        data: &[u8],
        now_ms: u64,
        link: &mut dyn Link,
        sockets: &mut dyn Sockets,
    ) -> bool {
        let Some(frame) = Frame::parse(data) else {
            return false;
        };
        if frame.destination != self.mac && frame.destination != MacAddr::BROADCAST {
            return false;
        }
        match frame.ethertype {
            ethernet::TYPE_ARP => self.handle_arp(frame.payload, link),
            ethernet::TYPE_IPV4 => self.handle_ipv4(&frame, now_ms, link, sockets),
            _ => false,
        }
    }

    fn handle_arp(&mut self, data: &[u8], link: &mut dyn Link) -> bool {
        let Some(packet) = arp::Packet::parse(data) else {
            return false;
        };
        let ours = self
            .config
            .is_some_and(|config| config.address == packet.target_ip);
        // RFC 826: a known sender is always brought up to date; an
        // unknown one is remembered only if it is talking to us.
        self.learn(packet.sender_ip, packet.sender_mac, ours);
        if ours && packet.operation == Operation::Request {
            self.send_arp(packet.sender_mac, packet.reply_from(self.mac), link);
        }
        ours
    }

    fn handle_ipv4(
        &mut self,
        frame: &Frame<'_>,
        now_ms: u64,
        link: &mut dyn Link,
        sockets: &mut dyn Sockets,
    ) -> bool {
        let Some(packet) = ipv4::Packet::parse(frame.payload) else {
            return false;
        };
        match packet.protocol {
            PROTOCOL_UDP => {
                let Some(datagram) =
                    Datagram::parse(packet.payload, packet.source, packet.destination)
                else {
                    return false;
                };
                // The DHCP server is listened for whatever address it
                // writes to: until there is an address, it is the only
                // thing that can be heard at all.
                if datagram.destination_port == dhcp::CLIENT_PORT {
                    return datagram.source_port == dhcp::SERVER_PORT
                        && self.handle_dhcp(datagram.payload, now_ms, link);
                }
                self.is_ours(packet.destination)
                    && sockets.udp(
                        packet.source,
                        datagram.source_port,
                        datagram.destination_port,
                        datagram.payload,
                    )
            }
            // Connections are between two hosts: a segment sent to a
            // broadcast address is not for anybody.
            PROTOCOL_TCP => self.config.is_some_and(|config| {
                packet.destination == config.address
                    && sockets.tcp(packet.source, packet.destination, packet.payload)
            }),
            PROTOCOL_ICMP => {
                let Some(config) = self.config else {
                    return false;
                };
                if packet.destination != config.address {
                    return false;
                }
                let Some(echo) = Echo::parse(packet.payload) else {
                    return false;
                };
                match echo.kind {
                    Kind::Reply => {
                        self.replies[self.replies_next] = Some(EchoReply {
                            from: packet.source,
                            identifier: echo.identifier,
                            sequence: echo.sequence,
                            ttl: packet.ttl,
                            length: echo.payload.len(),
                        });
                        self.replies_next = (self.replies_next + 1) % ECHO_REPLIES;
                    }
                    Kind::Request => {
                        // Answered to the station the request came from,
                        // which is the right next hop whether or not the
                        // requester is on this network.
                        let reply = Echo {
                            kind: Kind::Reply,
                            ..echo
                        };
                        let Some(length) =
                            reply.write(&mut self.frame[ethernet::HEADER_LEN + ipv4::HEADER_LEN..])
                        else {
                            return false;
                        };
                        self.send_ip(
                            frame.source,
                            PROTOCOL_ICMP,
                            config.address,
                            packet.source,
                            length,
                            link,
                        );
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn handle_dhcp(&mut self, data: &[u8], now_ms: u64, link: &mut dyn Link) -> bool {
        let Some(reply) = Reply::parse(data, self.transaction, self.mac) else {
            return false;
        };
        match (self.dhcp, reply.kind) {
            (DhcpState::Discovering, ReplyKind::Offer) => {
                let Some(server) = reply.server else {
                    return false;
                };
                self.lease = Some(Lease {
                    address: reply.address,
                    server,
                });
                self.dhcp = DhcpState::Requesting;
                self.attempts = 1;
                self.next_at_ms = now_ms + DHCP_RETRY_MS;
                self.send_dhcp(link);
                true
            }
            (DhcpState::Requesting, ReplyKind::Ack) => {
                self.config = Some(Config {
                    address: reply.address,
                    // Without a mask from the server, only the address
                    // itself is taken to be local.
                    mask: reply.mask.unwrap_or(Ipv4Addr::BROADCAST),
                    router: reply.router,
                    dns: reply.dns,
                    lease_seconds: reply.lease_seconds,
                });
                self.dhcp = DhcpState::Bound;
                true
            }
            (DhcpState::Requesting, ReplyKind::Nak) => {
                self.begin_discovery(now_ms);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::vec::Vec;

    const MAC: MacAddr = MacAddr([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
    const US: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
    const ROUTER: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);
    const ROUTER_MAC: MacAddr = MacAddr([0x52, 0x55, 10, 0, 2, 2]);
    const PEER: Ipv4Addr = Ipv4Addr([10, 0, 2, 77]);
    const PEER_MAC: MacAddr = MacAddr([2, 0, 0, 0, 0, 77]);
    const FAR: Ipv4Addr = Ipv4Addr([93, 184, 216, 34]);
    const MASK: Ipv4Addr = Ipv4Addr([255, 255, 255, 0]);

    /// Collects what the stack sends.
    #[derive(Default)]
    struct Wire(Vec<Vec<u8>>);

    impl Link for Wire {
        fn send(&mut self, frame: &[u8]) {
            assert!(
                (60..=1514).contains(&frame.len()),
                "frame of {} bytes",
                frame.len()
            );
            self.0.push(frame.to_vec());
        }
    }

    impl Wire {
        fn take(&mut self) -> Vec<Vec<u8>> {
            core::mem::take(&mut self.0)
        }

        /// The only frame sent since the last look.
        fn one(&mut self) -> Vec<u8> {
            let mut frames = self.take();
            assert_eq!(frames.len(), 1, "expected exactly one frame");
            frames.remove(0)
        }
    }

    fn configured() -> Stack {
        let mut stack = Stack::new(MAC, 1);
        stack.set_config(Config {
            address: US,
            mask: MASK,
            router: Some(ROUTER),
            dns: None,
            lease_seconds: None,
        });
        stack
    }

    fn ethernet_frame(to: MacAddr, from: MacAddr, ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut frame = std::vec![0u8; 14 + payload.len()];
        ethernet::write_header(&mut frame, to, from, ethertype).unwrap();
        frame[14..].copy_from_slice(payload);
        frame.resize(frame.len().max(60), 0);
        frame
    }

    fn arp_frame(to: MacAddr, packet: arp::Packet) -> Vec<u8> {
        let mut body = [0u8; 28];
        packet.write(&mut body).unwrap();
        ethernet_frame(to, packet.sender_mac, ethernet::TYPE_ARP, &body)
    }

    fn ip_frame(
        to: MacAddr,
        from: MacAddr,
        protocol: u8,
        source: Ipv4Addr,
        destination: Ipv4Addr,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = std::vec![0u8; 20 + payload.len()];
        ipv4::write_header(&mut packet, protocol, source, destination, 1, payload.len()).unwrap();
        packet[20..].copy_from_slice(payload);
        ethernet_frame(to, from, ethernet::TYPE_IPV4, &packet)
    }

    fn echo_bytes(kind: Kind, identifier: u16, sequence: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = std::vec![0u8; 8 + payload.len()];
        Echo {
            kind,
            identifier,
            sequence,
            payload,
        }
        .write(&mut bytes)
        .unwrap();
        bytes
    }

    /// Teaches the stack a neighbour the way the network would: by
    /// answering its ARP request.
    fn answer_arp(stack: &mut Stack, wire: &mut Wire, address: Ipv4Addr, mac: MacAddr) {
        let request = wire.one();
        let frame = Frame::parse(&request).unwrap();
        assert_eq!(frame.destination, MacAddr::BROADCAST);
        assert_eq!(frame.source, MAC);
        assert_eq!(frame.ethertype, ethernet::TYPE_ARP);
        let packet = arp::Packet::parse(frame.payload).unwrap();
        assert_eq!(
            packet,
            arp::Packet {
                operation: Operation::Request,
                sender_mac: MAC,
                sender_ip: US,
                target_mac: MacAddr::ZERO,
                target_ip: address,
            }
        );
        stack.receive(
            &arp_frame(MAC, packet.reply_from(mac)),
            0,
            wire,
            &mut NoSockets,
        );
        assert!(wire.take().is_empty(), "a reply is not answered");
    }

    #[test]
    fn nothing_is_sent_without_an_address() {
        let mut stack = Stack::new(MAC, 1);
        let mut wire = Wire::default();
        assert_eq!(
            stack.resolve(ROUTER, &mut wire),
            Err(SendError::NotConfigured)
        );
        assert_eq!(
            stack.send_echo(ROUTER, 1, 1, b"x", &mut wire),
            Err(SendError::NotConfigured)
        );
        stack.tick(5_000, &mut wire);
        assert!(wire.take().is_empty());
        assert_eq!(stack.dhcp_state(), DhcpState::Off);
    }

    #[test]
    fn ping_on_the_local_network_resolves_then_sends() {
        let mut stack = configured();
        let mut wire = Wire::default();
        assert_eq!(
            stack.send_echo(PEER, 0x4F4D, 1, b"data", &mut wire),
            Err(SendError::Unresolved)
        );
        answer_arp(&mut stack, &mut wire, PEER, PEER_MAC);
        assert_eq!(stack.send_echo(PEER, 0x4F4D, 1, b"data", &mut wire), Ok(()));

        let sent = wire.one();
        let frame = Frame::parse(&sent).unwrap();
        assert_eq!((frame.destination, frame.source), (PEER_MAC, MAC));
        let packet = ipv4::Packet::parse(frame.payload).unwrap();
        assert_eq!((packet.source, packet.destination), (US, PEER));
        assert_eq!(packet.protocol, PROTOCOL_ICMP);
        let echo = Echo::parse(packet.payload).unwrap();
        assert_eq!(
            echo,
            Echo {
                kind: Kind::Request,
                identifier: 0x4F4D,
                sequence: 1,
                payload: b"data"
            }
        );

        // Nothing has come back yet.
        assert_eq!(stack.take_echo_reply(PEER, 0x4F4D, 1), None);
        let reply = echo_bytes(Kind::Reply, 0x4F4D, 1, b"data");
        stack.receive(
            &ip_frame(MAC, PEER_MAC, PROTOCOL_ICMP, PEER, US, &reply),
            0,
            &mut wire,
            &mut NoSockets,
        );
        // Only the matching request finds it, and only once.
        assert_eq!(stack.take_echo_reply(PEER, 0x4F4D, 2), None);
        assert_eq!(stack.take_echo_reply(ROUTER, 0x4F4D, 1), None);
        assert_eq!(
            stack.take_echo_reply(PEER, 0x4F4D, 1),
            Some(EchoReply {
                from: PEER,
                identifier: 0x4F4D,
                sequence: 1,
                ttl: 64,
                length: 4
            })
        );
        assert_eq!(stack.take_echo_reply(PEER, 0x4F4D, 1), None);
        assert!(wire.take().is_empty());
    }

    #[test]
    fn packets_for_other_networks_go_to_the_router() {
        let mut stack = configured();
        let mut wire = Wire::default();
        assert_eq!(
            stack.send_echo(FAR, 1, 1, b"", &mut wire),
            Err(SendError::Unresolved)
        );
        // The question asked is about the router, not the destination.
        answer_arp(&mut stack, &mut wire, ROUTER, ROUTER_MAC);
        assert_eq!(stack.send_echo(FAR, 1, 1, b"", &mut wire), Ok(()));
        let sent = wire.one();
        let frame = Frame::parse(&sent).unwrap();
        assert_eq!(frame.destination, ROUTER_MAC);
        assert_eq!(ipv4::Packet::parse(frame.payload).unwrap().destination, FAR);

        let mut alone = configured();
        alone.set_config(Config {
            router: None,
            ..alone.config().unwrap()
        });
        assert_eq!(
            alone.send_echo(FAR, 1, 1, b"", &mut wire),
            Err(SendError::NoRoute)
        );
        assert!(wire.take().is_empty());
    }

    #[test]
    fn broadcast_addresses_need_no_resolution() {
        let mut stack = configured();
        let mut wire = Wire::default();
        assert_eq!(
            stack.resolve(Ipv4Addr::BROADCAST, &mut wire),
            Ok(MacAddr::BROADCAST)
        );
        assert_eq!(
            stack.resolve(Ipv4Addr([10, 0, 2, 255]), &mut wire),
            Ok(MacAddr::BROADCAST)
        );
        assert!(wire.take().is_empty());
        // 10.0.3.255 is somebody else's network: an ordinary remote address.
        assert_eq!(
            stack.resolve(Ipv4Addr([10, 0, 3, 255]), &mut wire),
            Err(SendError::Unresolved)
        );
    }

    #[test]
    fn oversized_echo_data_is_refused() {
        let mut stack = configured();
        let mut wire = Wire::default();
        answer_arp_after(&mut stack, &mut wire);
        let big = [0u8; 1473];
        assert_eq!(
            stack.send_echo(PEER, 1, 1, &big, &mut wire),
            Err(SendError::TooLong)
        );
        assert!(wire.take().is_empty());
        assert_eq!(stack.send_echo(PEER, 1, 1, &big[..1472], &mut wire), Ok(()));
        assert_eq!(wire.one().len(), 1514);
    }

    fn answer_arp_after(stack: &mut Stack, wire: &mut Wire) {
        assert_eq!(stack.resolve(PEER, wire), Err(SendError::Unresolved));
        answer_arp(stack, wire, PEER, PEER_MAC);
    }

    #[test]
    fn arp_requests_for_our_address_are_answered_and_the_asker_remembered() {
        let mut stack = configured();
        let mut wire = Wire::default();
        let request = arp::Packet {
            operation: Operation::Request,
            sender_mac: PEER_MAC,
            sender_ip: PEER,
            target_mac: MacAddr::ZERO,
            target_ip: US,
        };
        stack.receive(
            &arp_frame(MacAddr::BROADCAST, request),
            0,
            &mut wire,
            &mut NoSockets,
        );
        let sent = wire.one();
        let frame = Frame::parse(&sent).unwrap();
        assert_eq!((frame.destination, frame.source), (PEER_MAC, MAC));
        assert_eq!(
            arp::Packet::parse(frame.payload).unwrap(),
            arp::Packet {
                operation: Operation::Reply,
                sender_mac: MAC,
                sender_ip: US,
                target_mac: PEER_MAC,
                target_ip: PEER,
            }
        );
        // The asker can now be reached without asking back.
        assert_eq!(stack.resolve(PEER, &mut wire), Ok(PEER_MAC));
        assert!(wire.take().is_empty());
    }

    #[test]
    fn arp_between_other_stations_is_not_answered_or_remembered() {
        let mut stack = configured();
        let mut wire = Wire::default();
        let request = arp::Packet {
            operation: Operation::Request,
            sender_mac: PEER_MAC,
            sender_ip: PEER,
            target_mac: MacAddr::ZERO,
            target_ip: ROUTER,
        };
        stack.receive(
            &arp_frame(MacAddr::BROADCAST, request),
            0,
            &mut wire,
            &mut NoSockets,
        );
        assert!(wire.take().is_empty());
        assert_eq!(stack.neighbours().count(), 0);
        assert_eq!(stack.stats().ignored, 1);

        // But a station already known that announces a new hardware
        // address is brought up to date.
        answer_arp_after(&mut stack, &mut wire);
        let moved = MacAddr([2, 0, 0, 0, 0, 78]);
        let announcement = arp::Packet {
            sender_mac: moved,
            ..request
        };
        stack.receive(
            &arp_frame(MacAddr::BROADCAST, announcement),
            0,
            &mut wire,
            &mut NoSockets,
        );
        assert_eq!(stack.resolve(PEER, &mut wire), Ok(moved));
        assert_eq!(stack.neighbours().count(), 1);
    }

    #[test]
    fn a_full_neighbour_table_replaces_old_entries() {
        let mut stack = configured();
        let mut wire = Wire::default();
        for host in 100..140u8 {
            let address = Ipv4Addr([10, 0, 2, host]);
            let request = arp::Packet {
                operation: Operation::Request,
                sender_mac: MacAddr([2, 0, 0, 0, 0, host]),
                sender_ip: address,
                target_mac: MacAddr::ZERO,
                target_ip: US,
            };
            stack.receive(
                &arp_frame(MacAddr::BROADCAST, request),
                0,
                &mut wire,
                &mut NoSockets,
            );
            wire.take();
            assert_eq!(
                stack.resolve(address, &mut wire),
                Ok(MacAddr([2, 0, 0, 0, 0, host]))
            );
        }
        assert_eq!(stack.neighbours().count(), ARP_ENTRIES);
    }

    #[test]
    fn echo_requests_are_answered_with_the_same_data() {
        let mut stack = configured();
        let mut wire = Wire::default();
        // From another network, arriving through the router.
        let request = echo_bytes(Kind::Request, 7, 9, b"are you there?");
        stack.receive(
            &ip_frame(MAC, ROUTER_MAC, PROTOCOL_ICMP, FAR, US, &request),
            0,
            &mut wire,
            &mut NoSockets,
        );
        let sent = wire.one();
        let frame = Frame::parse(&sent).unwrap();
        assert_eq!((frame.destination, frame.source), (ROUTER_MAC, MAC));
        let packet = ipv4::Packet::parse(frame.payload).unwrap();
        assert_eq!((packet.source, packet.destination), (US, FAR));
        assert_eq!(
            Echo::parse(packet.payload).unwrap(),
            Echo {
                kind: Kind::Reply,
                identifier: 7,
                sequence: 9,
                payload: b"are you there?"
            }
        );
    }

    #[test]
    fn frames_that_are_not_for_us_are_counted_and_dropped() {
        let mut stack = configured();
        let mut wire = Wire::default();
        let request = echo_bytes(Kind::Request, 7, 9, b"x");
        let other_mac = MacAddr([2, 0, 0, 0, 0, 99]);
        let frames = [
            // Another station's hardware address.
            ip_frame(other_mac, PEER_MAC, PROTOCOL_ICMP, PEER, US, &request),
            // Our hardware address, another IP address.
            ip_frame(MAC, PEER_MAC, PROTOCOL_ICMP, PEER, ROUTER, &request),
            // A protocol nobody here speaks (GRE), and an unknown EtherType.
            ip_frame(MAC, PEER_MAC, 47, PEER, US, &request),
            ethernet_frame(MAC, PEER_MAC, 0x86DD, &[0; 40]),
            // UDP to a port nobody listens on.
            {
                let mut datagram = [0u8; 12];
                udp::finish(&mut datagram, PEER, 1000, US, 2000, 4).unwrap();
                ip_frame(MAC, PEER_MAC, PROTOCOL_UDP, PEER, US, &datagram)
            },
            // Too short to be a frame at all.
            std::vec![0u8; 10],
        ];
        for frame in &frames {
            stack.receive(frame, 0, &mut wire, &mut NoSockets);
        }
        // A damaged copy of a good frame.
        let mut damaged = ip_frame(MAC, PEER_MAC, PROTOCOL_ICMP, PEER, US, &request);
        damaged[40] ^= 1;
        stack.receive(&damaged, 0, &mut wire, &mut NoSockets);
        assert!(wire.take().is_empty());
        assert_eq!(
            stack.stats(),
            Stats {
                received: 7,
                ignored: 7,
                sent: 0
            }
        );
    }

    // --- DHCP, with the test playing the server ---

    /// The DHCP message in a frame the stack sent, after checking how it
    /// was addressed.
    fn dhcp_message(frame: &[u8]) -> Vec<u8> {
        let frame = Frame::parse(frame).unwrap();
        assert_eq!((frame.destination, frame.source), (MacAddr::BROADCAST, MAC));
        let packet = ipv4::Packet::parse(frame.payload).unwrap();
        assert_eq!(
            (packet.source, packet.destination),
            (Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST)
        );
        let datagram = Datagram::parse(packet.payload, packet.source, packet.destination).unwrap();
        assert_eq!((datagram.source_port, datagram.destination_port), (68, 67));
        assert_eq!(datagram.payload.len(), 300);
        datagram.payload.to_vec()
    }

    fn message_type(message: &[u8]) -> u8 {
        assert_eq!(message[240..242], [53, 1]);
        message[242]
    }

    fn transaction(message: &[u8]) -> u32 {
        u32::from_be_bytes([message[4], message[5], message[6], message[7]])
    }

    /// A frame carrying the server's answer of type `kind`.
    fn server_reply(kind: u8, transaction: u32, with_details: bool) -> Vec<u8> {
        let mut message = std::vec![0u8; 236];
        message[..3].copy_from_slice(&[2, 1, 6]);
        message[4..8].copy_from_slice(&transaction.to_be_bytes());
        message[16..20].copy_from_slice(&US.0);
        message[28..34].copy_from_slice(&MAC.0);
        message.extend_from_slice(&[99, 130, 83, 99, 53, 1, kind, 54, 4, 10, 0, 2, 2]);
        if with_details {
            message.extend_from_slice(&[1, 4, 255, 255, 255, 0, 3, 4, 10, 0, 2, 2]);
            message.extend_from_slice(&[6, 4, 10, 0, 2, 3, 51, 4, 0, 1, 0x51, 0x80]);
        }
        message.push(255);
        let mut datagram = std::vec![0u8; 8 + message.len()];
        datagram[8..].copy_from_slice(&message);
        udp::finish(
            &mut datagram,
            ROUTER,
            67,
            Ipv4Addr::BROADCAST,
            68,
            message.len(),
        )
        .unwrap();
        ip_frame(
            MacAddr::BROADCAST,
            ROUTER_MAC,
            PROTOCOL_UDP,
            ROUTER,
            Ipv4Addr::BROADCAST,
            &datagram,
        )
    }

    #[test]
    fn dhcp_obtains_an_address() {
        let mut stack = Stack::new(MAC, 42);
        let mut wire = Wire::default();
        stack.start_dhcp();
        assert_eq!(stack.dhcp_state(), DhcpState::Discovering);
        assert!(
            wire.take().is_empty(),
            "nothing is sent before the first tick"
        );

        stack.tick(100, &mut wire);
        let discover = dhcp_message(&wire.one());
        assert_eq!(message_type(&discover), 1);
        let xid = transaction(&discover);

        stack.receive(&server_reply(2, xid, true), 120, &mut wire, &mut NoSockets);
        assert_eq!(stack.dhcp_state(), DhcpState::Requesting);
        let request = dhcp_message(&wire.one());
        assert_eq!(message_type(&request), 3);
        assert_eq!(transaction(&request), xid);
        // Requested address and server identifier, as offered.
        assert_eq!(request[243..255], [50, 4, 10, 0, 2, 15, 54, 4, 10, 0, 2, 2]);
        assert_eq!(stack.config(), None, "an offer is not yet an address");

        stack.receive(&server_reply(5, xid, true), 140, &mut wire, &mut NoSockets);
        assert_eq!(stack.dhcp_state(), DhcpState::Bound);
        assert_eq!(
            stack.config(),
            Some(Config {
                address: US,
                mask: MASK,
                router: Some(ROUTER),
                dns: Some(Ipv4Addr([10, 0, 2, 3])),
                lease_seconds: Some(86_400),
            })
        );
        // Bound: time passing sends nothing more.
        stack.tick(100_000, &mut wire);
        assert!(wire.take().is_empty());
    }

    #[test]
    fn dhcp_repeats_itself_and_then_gives_up() {
        let mut stack = Stack::new(MAC, 42);
        let mut wire = Wire::default();
        stack.start_dhcp();
        let mut sent = 0;
        // Ticks every 10 ms for a minute.
        for now in (0..60_000).step_by(10) {
            stack.tick(now, &mut wire);
            for frame in wire.take() {
                assert_eq!(message_type(&dhcp_message(&frame)), 1);
                sent += 1;
                assert_eq!(now, (sent - 1) * DHCP_RETRY_MS, "one message a second");
            }
        }
        assert_eq!(sent, u64::from(DHCP_ATTEMPTS));
        assert_eq!(stack.dhcp_state(), DhcpState::Failed);
        assert_eq!(stack.config(), None);
        // It can be started again, with a new transaction number.
        stack.start_dhcp();
        stack.tick(60_000, &mut wire);
        assert_eq!(message_type(&dhcp_message(&wire.one())), 1);
    }

    #[test]
    fn dhcp_ignores_answers_to_other_questions() {
        let mut stack = Stack::new(MAC, 42);
        let mut wire = Wire::default();
        stack.start_dhcp();
        stack.tick(0, &mut wire);
        let xid = transaction(&dhcp_message(&wire.one()));
        // Another transaction; an acknowledgement nobody asked for yet.
        stack.receive(
            &server_reply(2, xid ^ 1, true),
            10,
            &mut wire,
            &mut NoSockets,
        );
        stack.receive(&server_reply(5, xid, true), 10, &mut wire, &mut NoSockets);
        assert_eq!(stack.dhcp_state(), DhcpState::Discovering);
        assert_eq!(stack.config(), None);
        assert!(wire.take().is_empty());
        assert_eq!(stack.stats().ignored, 2);
    }

    #[test]
    fn a_refused_request_starts_over() {
        let mut stack = Stack::new(MAC, 42);
        let mut wire = Wire::default();
        stack.start_dhcp();
        stack.tick(0, &mut wire);
        let first = transaction(&dhcp_message(&wire.one()));
        stack.receive(&server_reply(2, first, true), 10, &mut wire, &mut NoSockets);
        wire.take();
        stack.receive(
            &server_reply(6, first, false),
            20,
            &mut wire,
            &mut NoSockets,
        );
        assert_eq!(stack.dhcp_state(), DhcpState::Discovering);
        stack.tick(20, &mut wire);
        let again = dhcp_message(&wire.one());
        assert_eq!(message_type(&again), 1);
        assert_ne!(transaction(&again), first);
    }

    #[test]
    fn a_silent_server_after_its_offer_means_starting_over() {
        let mut stack = Stack::new(MAC, 42);
        let mut wire = Wire::default();
        stack.start_dhcp();
        stack.tick(0, &mut wire);
        let xid = transaction(&dhcp_message(&wire.one()));
        stack.receive(&server_reply(2, xid, true), 0, &mut wire, &mut NoSockets);
        let mut kinds = Vec::new();
        kinds.push(message_type(&dhcp_message(&wire.one())));
        for now in (0..20_000).step_by(10) {
            stack.tick(now, &mut wire);
            for frame in wire.take() {
                kinds.push(message_type(&dhcp_message(&frame)));
            }
        }
        // Five requests, then five discoveries, then silence.
        assert_eq!(kinds, [3, 3, 3, 3, 3, 1, 1, 1, 1, 1]);
        assert_eq!(stack.dhcp_state(), DhcpState::Failed);
    }

    #[test]
    fn an_acknowledgement_without_a_mask_makes_nothing_local() {
        let mut stack = Stack::new(MAC, 42);
        let mut wire = Wire::default();
        stack.start_dhcp();
        stack.tick(0, &mut wire);
        let xid = transaction(&dhcp_message(&wire.one()));
        stack.receive(&server_reply(2, xid, false), 0, &mut wire, &mut NoSockets);
        wire.take();
        stack.receive(&server_reply(5, xid, false), 0, &mut wire, &mut NoSockets);
        let config = stack.config().unwrap();
        assert_eq!(config.mask, Ipv4Addr::BROADCAST);
        assert_eq!(config.router, None);
        assert_eq!(stack.resolve(PEER, &mut wire), Err(SendError::NoRoute));
        // And its own address is not mistaken for a broadcast address.
        assert_eq!(stack.resolve(US, &mut wire), Err(SendError::Unresolved));
    }

    // --- UDP for local ports ---

    /// Listens on port 4000 and records what arrives.
    #[derive(Default)]
    struct Listener(Vec<(Ipv4Addr, u16, u16, Vec<u8>)>);

    impl Sockets for Listener {
        fn udp(&mut self, from: Ipv4Addr, from_port: u16, port: u16, data: &[u8]) -> bool {
            if port != 4000 {
                return false;
            }
            self.0.push((from, from_port, port, data.to_vec()));
            true
        }

        fn tcp(&mut self, from: Ipv4Addr, to: Ipv4Addr, segment: &[u8]) -> bool {
            // Recorded with port numbers of zero, to tell them apart.
            assert_eq!(to, US);
            self.0.push((from, 0, 0, segment.to_vec()));
            true
        }
    }

    fn udp_frame(to: MacAddr, source: Ipv4Addr, destination: Ipv4Addr, port: u16) -> Vec<u8> {
        let mut datagram = [0u8; 13];
        datagram[8..].copy_from_slice(b"hello");
        udp::finish(&mut datagram, source, 1234, destination, port, 5).unwrap();
        ip_frame(to, PEER_MAC, PROTOCOL_UDP, source, destination, &datagram)
    }

    #[test]
    fn udp_is_sent_with_addresses_ports_and_checksum() {
        let mut stack = configured();
        let mut wire = Wire::default();
        assert_eq!(
            stack.send_udp(PEER, 4000, 69, b"request", &mut wire),
            Err(SendError::Unresolved)
        );
        answer_arp(&mut stack, &mut wire, PEER, PEER_MAC);
        assert_eq!(
            stack.send_udp(PEER, 4000, 69, b"request", &mut wire),
            Ok(())
        );
        let sent = wire.one();
        let frame = Frame::parse(&sent).unwrap();
        assert_eq!(frame.destination, PEER_MAC);
        let packet = ipv4::Packet::parse(frame.payload).unwrap();
        assert_eq!((packet.source, packet.destination), (US, PEER));
        // Parsing verifies the checksum, which covers the addresses.
        let datagram = Datagram::parse(packet.payload, US, PEER).unwrap();
        assert_eq!(
            (datagram.source_port, datagram.destination_port),
            (4000, 69)
        );
        assert_eq!(datagram.payload, b"request");
        assert_ne!(packet.payload[6..8], [0, 0], "a checksum is always sent");
    }

    #[test]
    fn udp_limits() {
        let mut stack = configured();
        let mut wire = Wire::default();
        answer_arp_after(&mut stack, &mut wire);
        let big = [7u8; MAX_UDP_PAYLOAD + 1];
        assert_eq!(MAX_UDP_PAYLOAD, 1472);
        assert_eq!(
            stack.send_udp(PEER, 1, 2, &big, &mut wire),
            Err(SendError::TooLong)
        );
        assert_eq!(stack.send_udp(PEER, 1, 2, &big[..1472], &mut wire), Ok(()));
        assert_eq!(wire.one().len(), 1514);
        assert_eq!(stack.send_udp(PEER, 1, 2, b"", &mut wire), Ok(()));
        assert_eq!(
            wire.one().len(),
            60,
            "an empty datagram is padded to a full frame"
        );
        let mut bare = Stack::new(MAC, 1);
        assert_eq!(
            bare.send_udp(PEER, 1, 2, b"x", &mut wire),
            Err(SendError::NotConfigured)
        );
    }

    #[test]
    fn udp_reaches_the_listener_on_its_port_only() {
        let mut stack = configured();
        let mut wire = Wire::default();
        let mut listener = Listener::default();
        // To our address, to the network's broadcast address, to everyone.
        for destination in [US, Ipv4Addr([10, 0, 2, 255]), Ipv4Addr::BROADCAST] {
            let frame = udp_frame(MacAddr::BROADCAST, PEER, destination, 4000);
            stack.receive(&frame, 0, &mut wire, &mut listener);
        }
        assert_eq!(listener.0.len(), 3);
        assert_eq!(listener.0[0], (PEER, 1234, 4000, b"hello".to_vec()));
        assert_eq!(stack.stats().ignored, 0);

        // Another port; another host's address; a damaged datagram.
        stack.receive(&udp_frame(MAC, PEER, US, 4001), 0, &mut wire, &mut listener);
        stack.receive(
            &udp_frame(MAC, PEER, ROUTER, 4000),
            0,
            &mut wire,
            &mut listener,
        );
        let mut damaged = udp_frame(MAC, PEER, US, 4000);
        damaged[14 + 20 + 9] ^= 1;
        stack.receive(&damaged, 0, &mut wire, &mut listener);
        assert_eq!(listener.0.len(), 3);
        assert_eq!(stack.stats().ignored, 3);
        assert!(wire.take().is_empty(), "nothing is sent in answer");
    }

    #[test]
    fn without_an_address_only_dhcp_is_heard() {
        let mut stack = Stack::new(MAC, 1);
        let mut wire = Wire::default();
        let mut listener = Listener::default();
        let frame = udp_frame(MacAddr::BROADCAST, PEER, Ipv4Addr::BROADCAST, 4000);
        stack.receive(&frame, 0, &mut wire, &mut listener);
        assert!(listener.0.is_empty());
        // And a listener cannot take the DHCP client's port.
        let mut stack = configured();
        let frame = udp_frame(MAC, PEER, US, 68);
        stack.receive(&frame, 0, &mut wire, &mut listener);
        assert!(listener.0.is_empty());
    }

    // --- TCP segments pass through ---

    #[test]
    fn tcp_segments_are_passed_up_and_sent_down_unchanged() {
        use crate::tcp::{SYN, Segment};
        let mut stack = configured();
        let mut wire = Wire::default();
        let mut listener = Listener::default();
        let segment = Segment {
            source_port: 40000,
            destination_port: 7,
            sequence: 1,
            acknowledgement: 0,
            flags: SYN,
            window: 100,
            mss: None,
            payload: b"",
        };
        let mut bytes = [0u8; 20];
        segment.write(&mut bytes, PEER, US).unwrap();
        let frame = ip_frame(MAC, PEER_MAC, PROTOCOL_TCP, PEER, US, &bytes);
        stack.receive(&frame, 0, &mut wire, &mut listener);
        assert_eq!(listener.0, [(PEER, 0, 0, bytes.to_vec())]);
        // Not to a broadcast address, and not without anybody to take it.
        let broadcast = Ipv4Addr([10, 0, 2, 255]);
        let frame = ip_frame(
            MacAddr::BROADCAST,
            PEER_MAC,
            PROTOCOL_TCP,
            PEER,
            broadcast,
            &bytes,
        );
        stack.receive(&frame, 0, &mut wire, &mut listener);
        let frame = ip_frame(MAC, PEER_MAC, PROTOCOL_TCP, PEER, US, &bytes);
        stack.receive(&frame, 0, &mut wire, &mut NoSockets);
        assert_eq!(listener.0.len(), 1);
        assert_eq!(stack.stats().ignored, 2);

        // Sending: resolved like everything else, carried as protocol 6.
        assert_eq!(
            stack.send_tcp(PEER, &bytes, &mut wire),
            Err(SendError::Unresolved)
        );
        answer_arp(&mut stack, &mut wire, PEER, PEER_MAC);
        assert_eq!(stack.send_tcp(PEER, &bytes, &mut wire), Ok(()));
        let sent = wire.one();
        let packet = ipv4::Packet::parse(Frame::parse(&sent).unwrap().payload).unwrap();
        assert_eq!(
            (packet.protocol, packet.source, packet.destination),
            (6, US, PEER)
        );
        assert_eq!(packet.payload, bytes);
        assert_eq!(
            stack.send_tcp(PEER, &[0u8; 1481], &mut wire),
            Err(SendError::TooLong)
        );
    }

    #[test]
    fn arp_requests_for_one_address_are_not_repeated_at_once() {
        let mut stack = configured();
        let mut wire = Wire::default();
        let mut asked = Vec::new();
        // Something wants to reach the peer every 10 ms for a second.
        for now in (0..1000).step_by(10) {
            stack.tick(now, &mut wire);
            assert_eq!(stack.resolve(PEER, &mut wire), Err(SendError::Unresolved));
            if !wire.take().is_empty() {
                asked.push(now);
            }
        }
        assert_eq!(asked, [0, 200, 400, 600, 800]);
        // Another address is asked for regardless.
        assert_eq!(
            stack.resolve(Ipv4Addr([10, 0, 2, 78]), &mut wire),
            Err(SendError::Unresolved)
        );
        assert_eq!(wire.take().len(), 1);
    }
}
