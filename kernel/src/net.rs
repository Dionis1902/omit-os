// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! The machine's network interface: a card driver below, the protocol
//! logic of the `omit_net` crate above, and the clock and threads they
//! need.
//!
//! There is one interface, `eth0`: the first card a driver registers.
//! Cards are not interrupt driven: a kernel thread looks for received
//! frames a hundred times a second, and whoever waits for an answer
//! (see [`ping`]) looks continuously while it waits.
//!
//! What exists so far is the foundation — an address by DHCP, ARP,
//! answering and sending echo requests. There are no sockets yet, so
//! programs cannot use the network.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use omit_net::ethernet::MAX_FRAME_LEN;
use omit_net::stack::{DhcpState, Link, SendError, Stack};
use omit_net::{Ipv4Addr, MacAddr};

use crate::sync::Mutex;
use crate::{info, sched, time, warn};

/// Why a frame could not be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// Longer than an Ethernet frame.
    TooLong,
    /// The card did not take the frame in time.
    Timeout,
}

/// A network card.
pub trait NetDevice: Send {
    /// The card's hardware address.
    fn mac(&self) -> MacAddr;
    /// Sends one Ethernet frame (without check sequence), returning once
    /// the card has taken it.
    fn transmit(&mut self, frame: &[u8]) -> Result<(), NetError>;
    /// Copies the next received frame into `buffer` and returns its
    /// length, or `None` if nothing is waiting.
    fn receive(&mut self, buffer: &mut [u8]) -> Option<usize>;
}

struct Interface {
    driver: &'static str,
    device: Box<dyn NetDevice>,
    stack: Box<Stack>,
    /// Frames the card refused or did not take.
    transmit_errors: u64,
    /// The DHCP state last reported in the log.
    reported: DhcpState,
    buffer: Box<[u8; MAX_FRAME_LEN]>,
}

/// Sends the stack's frames through the card.
struct DeviceLink<'a> {
    device: &'a mut dyn NetDevice,
    errors: &'a mut u64,
}

impl Link for DeviceLink<'_> {
    fn send(&mut self, frame: &[u8]) {
        if self.device.transmit(frame).is_err() {
            *self.errors += 1;
        }
    }
}

static INTERFACE: Mutex<Option<Interface>> = Mutex::new(None);

/// Name of the one interface.
const NAME: &str = "eth0";
/// Identifier of this system's echo requests ("OM").
const ECHO_IDENTIFIER: u16 = 0x4F4D;
/// Data carried by an echo request.
const ECHO_PAYLOAD: [u8; 32] = *b"Omit OS echo request ...........";
/// How long `ping` waits for an answer.
const PING_TIMEOUT_NS: u64 = 1_000_000_000;
/// Pause between attempts to send while the next hop is being resolved.
const RESOLVE_RETRY_NS: u64 = 200_000_000;

fn now_ms() -> u64 {
    time::monotonic_ns() / 1_000_000
}

impl Interface {
    /// Takes in every frame the card has received and does what is due.
    fn poll(&mut self) {
        let now = now_ms();
        let mut link = DeviceLink {
            device: &mut *self.device,
            errors: &mut self.transmit_errors,
        };
        while let Some(length) = link.device.receive(&mut self.buffer[..]) {
            self.stack.receive(&self.buffer[..length], now, &mut link);
        }
        self.stack.tick(now, &mut link);

        let state = self.stack.dhcp_state();
        if state != self.reported {
            self.reported = state;
            match (state, self.stack.config()) {
                (DhcpState::Bound, Some(_)) => {
                    info!("net: {NAME}: {} (DHCP)", self.describe_address());
                }
                (DhcpState::Failed, _) => {
                    info!("net: {NAME}: no DHCP server answered; no address");
                }
                _ => {}
            }
        }
    }

    /// The addresses in one line, or what stands in for them.
    fn describe_address(&self) -> String {
        let Some(config) = self.stack.config() else {
            return match self.stack.dhcp_state() {
                DhcpState::Failed => "no address (no DHCP server answered)".into(),
                _ => "no address yet".into(),
            };
        };
        let mut line = format!("address {}", config.address);
        if let Some(prefix) = config.mask.prefix_length() {
            line.push_str(&format!("/{prefix}"));
        }
        if let Some(router) = config.router {
            line.push_str(&format!(", router {router}"));
        }
        if let Some(dns) = config.dns {
            line.push_str(&format!(", name server {dns}"));
        }
        line
    }
}

/// Makes `device` the machine's network interface, unless it already has
/// one, and starts looking for an address.
pub fn register(driver: &'static str, device: Box<dyn NetDevice>) {
    let mut interface = INTERFACE.lock();
    if interface.is_some() {
        warn!("net: a second network card ({driver}) is not used");
        return;
    }
    let mac = device.mac();
    // The DHCP transaction number should differ between machines and
    // from one start to the next; nothing depends on it being secret.
    let seed = u32::from_be_bytes([mac.0[2], mac.0[3], mac.0[4], mac.0[5]])
        ^ time::monotonic_ns() as u32
        ^ time::realtime_us().unwrap_or(0) as u32;
    let mut stack = Box::new(Stack::new(mac, seed));
    stack.start_dhcp();
    *interface = Some(Interface {
        driver,
        device,
        stack,
        transmit_errors: 0,
        reported: DhcpState::Discovering,
        buffer: Box::new([0; MAX_FRAME_LEN]),
    });
    drop(interface);
    if let Err(error) = sched::spawn("net", poll_thread, 0) {
        warn!("net: cannot start the receive thread: {error:?}");
    }
}

fn poll_thread(_: usize) {
    loop {
        if let Some(interface) = INTERFACE.lock().as_mut() {
            interface.poll();
        }
        sched::sleep_ticks(1);
    }
}

/// Waits, for at most `ticks` timer ticks, until the interface knows
/// whether DHCP gave it an address. Returns at once if there is no
/// interface.
pub fn wait_for_address(ticks: u64) {
    let deadline = time::ticks() + ticks;
    loop {
        match INTERFACE.lock().as_mut() {
            None => return,
            Some(interface) => {
                interface.poll();
                if matches!(
                    interface.stack.dhcp_state(),
                    DhcpState::Bound | DhcpState::Failed | DhcpState::Off
                ) {
                    return;
                }
            }
        }
        if time::ticks() >= deadline {
            return;
        }
        sched::sleep_ticks(1);
    }
}

/// Lines describing the interface, for `ifconfig`.
pub fn describe() -> Vec<String> {
    let guard = INTERFACE.lock();
    let Some(interface) = guard.as_ref() else {
        return alloc::vec!["no network interface".into()];
    };
    let stats = interface.stack.stats();
    let mut lines = alloc::vec![
        format!(
            "{NAME}: {}, hardware address {}",
            interface.driver,
            interface.stack.mac()
        ),
        format!("    {}", interface.describe_address()),
        format!(
            "    frames: {} received ({} ignored), {} sent ({} failed)",
            stats.received, stats.ignored, stats.sent, interface.transmit_errors
        ),
    ];
    for (address, mac) in interface.stack.neighbours() {
        lines.push(format!("    neighbour {address} is at {mac}"));
    }
    lines
}

/// An answer to [`ping`].
#[derive(Debug, Clone, Copy)]
pub struct PingReply {
    /// Time between sending the request and seeing the reply.
    pub micros: u64,
    /// Hops the reply had left.
    pub ttl: u8,
    /// Bytes of data returned.
    pub bytes: usize,
}

/// Why [`ping`] got no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingError {
    /// The machine has no network card with a driver.
    NoInterface,
    /// The interface has no address.
    NotConfigured,
    /// The destination is this machine; nothing is looped back.
    Ourselves,
    /// Another network, and no router is known.
    NoRoute,
    /// Nobody on the local network answered for the next hop's address.
    Unreachable,
    /// The request was sent and no reply came within a second.
    Timeout,
}

impl fmt::Display for PingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoInterface => "no network interface",
            Self::NotConfigured => "the network interface has no address",
            Self::Ourselves => "that is this machine's own address, which cannot be pinged yet",
            Self::NoRoute => "no route: another network, and no router is known",
            Self::Unreachable => "unreachable: nobody on the local network answers for it",
            Self::Timeout => "no answer within a second",
        })
    }
}

/// Sends one echo request to `destination` and waits up to a second for
/// the reply.
pub fn ping(destination: Ipv4Addr, sequence: u16) -> Result<PingReply, PingError> {
    let started = time::monotonic_ns();
    let mut sent_at = None;
    let mut next_attempt = started;
    loop {
        let now = time::monotonic_ns();
        {
            let mut guard = INTERFACE.lock();
            let interface = guard.as_mut().ok_or(PingError::NoInterface)?;
            interface.poll();
            let config = interface.stack.config().ok_or(PingError::NotConfigured)?;
            if config.address == destination {
                return Err(PingError::Ourselves);
            }
            if let Some(sent) = sent_at {
                if let Some(reply) =
                    interface
                        .stack
                        .take_echo_reply(destination, ECHO_IDENTIFIER, sequence)
                {
                    return Ok(PingReply {
                        micros: (time::monotonic_ns() - sent) / 1000,
                        ttl: reply.ttl,
                        bytes: reply.length,
                    });
                }
            } else if now >= next_attempt {
                next_attempt = now + RESOLVE_RETRY_NS;
                let Interface {
                    device,
                    stack,
                    transmit_errors,
                    ..
                } = interface;
                let mut link = DeviceLink {
                    device: &mut **device,
                    errors: transmit_errors,
                };
                match stack.send_echo(
                    destination,
                    ECHO_IDENTIFIER,
                    sequence,
                    &ECHO_PAYLOAD,
                    &mut link,
                ) {
                    Ok(()) => sent_at = Some(time::monotonic_ns()),
                    // An ARP request is on its way; try again shortly.
                    Err(SendError::Unresolved) => {}
                    Err(SendError::NotConfigured) => return Err(PingError::NotConfigured),
                    Err(SendError::NoRoute) => return Err(PingError::NoRoute),
                    Err(SendError::TooLong) => return Err(PingError::Timeout),
                }
            }
        }
        match sent_at {
            Some(sent) if now.saturating_sub(sent) > PING_TIMEOUT_NS => {
                return Err(PingError::Timeout);
            }
            None if now - started > PING_TIMEOUT_NS => return Err(PingError::Unreachable),
            _ => {}
        }
        sched::yield_now();
    }
}
