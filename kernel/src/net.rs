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
//! answering and sending echo requests — and sockets for programs: UDP
//! ([`UdpSocket`]) and TCP ([`tcp`]).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU16, Ordering};

use omit_abi::net::{INFO_CONFIGURED, INFO_NAME_SERVER, INFO_PRESENT, INFO_ROUTER, NetInfo};
use omit_net::ethernet::MAX_FRAME_LEN;
use omit_net::stack::{DhcpState, Link, SendError, Sockets, Stack};
use omit_net::{Ipv4Addr, MacAddr, dhcp};

use crate::sched::WaitQueue;
use crate::sync::{Mutex, SpinLock};
use crate::{info, poll, sched, time, warn};

pub mod tcp;

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

pub(crate) fn now_ms() -> u64 {
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
        let mut sockets = KernelSockets::default();
        while let Some(length) = link.device.receive(&mut self.buffer[..]) {
            self.stack
                .receive(&self.buffer[..length], now, &mut link, &mut sockets);
        }
        self.stack.tick(now, &mut link);
        tcp::pump(&mut self.stack, &mut link, sockets.resets);

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

// ---------------------------------------------------------------------
// UDP sockets

/// Datagrams a socket keeps for its owner; further ones are dropped.
const SOCKET_QUEUE_MAX: usize = 32;
/// Ports handed out when the program does not ask for a particular one.
const EPHEMERAL_PORTS: core::ops::RangeInclusive<u16> = 49152..=65535;
/// How long a receiver looks for frames itself before it goes to sleep
/// and leaves that to the receive thread. Answers to something just sent
/// usually arrive within this time, and are then not delayed by up to a
/// timer tick.
const EAGER_RECEIVE_NS: u64 = 20_000_000;

/// How a wait ended.
pub(crate) enum Waited {
    /// What was waited for is there.
    Ready,
    /// The time allowed has passed.
    TimedOut,
    /// The waiting thread's process is being terminated.
    Killed,
}

/// Waits until `ready` holds, for at most `timeout_ticks` timer ticks
/// (`None`: for ever). `queue` must be woken when `ready` may have
/// changed; `ready` must not block.
///
/// For the first [`EAGER_RECEIVE_NS`] the waiting thread looks for
/// received frames itself; after that it sleeps and leaves that to the
/// receive thread.
pub(crate) fn wait_until(
    queue: &WaitQueue,
    timeout_ticks: Option<u64>,
    mut ready: impl FnMut() -> bool,
) -> Waited {
    let started = time::monotonic_ns();
    let deadline = timeout_ticks.map(|ticks| time::ticks() + ticks);
    let expired = || deadline.is_some_and(|deadline| time::ticks() >= deadline);
    loop {
        if ready() {
            return Waited::Ready;
        }
        if sched::current_killed() {
            return Waited::Killed;
        }
        if expired() {
            return Waited::TimedOut;
        }
        if time::monotonic_ns() - started < EAGER_RECEIVE_NS {
            kick();
            sched::yield_now();
        } else {
            let remaining = deadline.map_or(u64::MAX / 2, |deadline| {
                deadline.saturating_sub(time::ticks())
            });
            queue.wait_until_or_timeout(|| ready() || sched::current_killed(), remaining);
        }
    }
}

/// Has the interface take in what has arrived and send what is due,
/// now rather than at the receive thread's next turn.
pub(crate) fn kick() {
    if let Some(interface) = INTERFACE.lock().as_mut() {
        interface.poll();
    }
}

/// Waits up to a second for the hardware address of the next hop
/// towards `destination` to be known. `Err(true)`: the thread was
/// killed; `Err(false)`: there is no way there.
pub(crate) fn wait_resolved(destination: Ipv4Addr) -> Result<(), bool> {
    let started = time::monotonic_ns();
    loop {
        {
            let mut guard = INTERFACE.lock();
            let interface = guard.as_mut().ok_or(false)?;
            interface.poll();
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
            match stack.resolve(destination, &mut link) {
                Ok(_) => return Ok(()),
                Err(SendError::Unresolved) => {}
                Err(_) => return Err(false),
            }
        }
        if time::monotonic_ns() - started > PING_TIMEOUT_NS {
            return Err(false);
        }
        if sched::current_killed() {
            return Err(true);
        }
        sched::yield_now();
    }
}

/// A received datagram.
pub struct Received {
    /// The sender's address.
    pub from: Ipv4Addr,
    /// The sender's port.
    pub from_port: u16,
    /// The data.
    pub data: Vec<u8>,
}

/// A UDP socket: a local port and the datagrams received on it.
pub struct UdpSocket {
    port: u16,
    queue: SpinLock<VecDeque<Received>>,
    readers: WaitQueue,
}

/// Every open socket, with its port, so that a socket is only ever
/// referenced when it is the one wanted. Entries whose socket is gone are
/// removed when the socket is dropped.
static SOCKETS: SpinLock<Vec<(u16, Weak<UdpSocket>)>> = SpinLock::new(Vec::new());
static NEXT_EPHEMERAL: AtomicU16 = AtomicU16::new(*EPHEMERAL_PORTS.start());

/// Why a socket operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpError {
    /// The port is taken (or no port is free).
    PortInUse,
    /// More data than fits one datagram.
    TooLong,
    /// No interface, no address, no route, or nobody answers for the
    /// next hop.
    Unreachable,
    /// Nothing arrived in the time allowed.
    TimedOut,
    /// The waiting thread's process is being terminated.
    Killed,
}

fn socket_on(sockets: &[(u16, Weak<UdpSocket>)], port: u16) -> Option<Arc<UdpSocket>> {
    sockets
        .iter()
        .find(|(taken, _)| *taken == port)
        .and_then(|(_, socket)| socket.upgrade())
}

impl UdpSocket {
    /// Opens a socket on `port`, or on a free port if `port` is 0.
    pub fn bind(port: u16) -> Result<Arc<Self>, UdpError> {
        // Looked up and dropped outside the table's lock: dropping the
        // last reference to a socket takes that lock.
        let existing = |port| socket_on(&SOCKETS.lock(), port).is_some();
        let port = if port == 0 {
            let mut candidates = (0..EPHEMERAL_PORTS.len()).map(|_| {
                let next = NEXT_EPHEMERAL.fetch_add(1, Ordering::Relaxed);
                if next == *EPHEMERAL_PORTS.end() {
                    NEXT_EPHEMERAL.store(*EPHEMERAL_PORTS.start(), Ordering::Relaxed);
                }
                next.max(*EPHEMERAL_PORTS.start())
            });
            candidates
                .find(|&port| !existing(port))
                .ok_or(UdpError::PortInUse)?
        } else if port == dhcp::CLIENT_PORT || existing(port) {
            // The DHCP client's port is the kernel's own.
            return Err(UdpError::PortInUse);
        } else {
            port
        };
        let socket = Arc::new(Self {
            port,
            queue: SpinLock::new(VecDeque::new()),
            readers: WaitQueue::new(),
        });
        let mut sockets = SOCKETS.lock();
        // Two threads may have chosen the same port at the same moment.
        let taken = sockets
            .iter()
            .any(|(taken, entry)| *taken == port && entry.strong_count() > 0);
        if taken {
            drop(sockets);
            return Err(UdpError::PortInUse);
        }
        sockets.push((port, Arc::downgrade(&socket)));
        Ok(socket)
    }

    /// Whether a datagram is waiting.
    pub fn ready(&self) -> bool {
        !self.queue.lock().is_empty()
    }

    /// Sends `data` to `to`, port `to_port`. Waits up to a second for
    /// the next hop's hardware address if it is not known yet.
    pub fn send(&self, to: Ipv4Addr, to_port: u16, data: &[u8]) -> Result<(), UdpError> {
        let started = time::monotonic_ns();
        let mut next_attempt = started;
        loop {
            let now = time::monotonic_ns();
            {
                let mut guard = INTERFACE.lock();
                let interface = guard.as_mut().ok_or(UdpError::Unreachable)?;
                interface.poll();
                if now >= next_attempt {
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
                    match stack.send_udp(to, self.port, to_port, data, &mut link) {
                        Ok(()) => return Ok(()),
                        // An ARP request is on its way; try again shortly.
                        Err(SendError::Unresolved) => {}
                        Err(SendError::TooLong) => return Err(UdpError::TooLong),
                        Err(SendError::NotConfigured | SendError::NoRoute) => {
                            return Err(UdpError::Unreachable);
                        }
                    }
                }
            }
            if now - started > PING_TIMEOUT_NS {
                return Err(UdpError::Unreachable);
            }
            if sched::current_killed() {
                return Err(UdpError::Killed);
            }
            sched::yield_now();
        }
    }

    /// Takes the next received datagram, waiting up to `timeout_ticks`
    /// timer ticks (`None`: for ever).
    pub fn receive(&self, timeout_ticks: Option<u64>) -> Result<Received, UdpError> {
        let take = || self.queue.lock().pop_front();
        if let Some(received) = take() {
            return Ok(received);
        }
        if timeout_ticks == Some(0) {
            // One look at the card, so that a caller that only ever
            // asks without waiting still sees what has arrived.
            if let Some(interface) = INTERFACE.lock().as_mut() {
                interface.poll();
            }
            return take().ok_or(UdpError::TimedOut);
        }
        loop {
            match wait_until(&self.readers, timeout_ticks, || self.ready()) {
                Waited::Ready => {}
                Waited::TimedOut => return Err(UdpError::TimedOut),
                Waited::Killed => return Err(UdpError::Killed),
            }
            // Another thread reading the same socket may have been quicker.
            if let Some(received) = take() {
                return Ok(received);
            }
            if timeout_ticks.is_some() {
                return Err(UdpError::TimedOut);
            }
        }
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        // This socket's own entry can no longer be upgraded.
        SOCKETS.lock().retain(|(_, entry)| entry.strong_count() > 0);
    }
}

/// Hands what arrives to the sockets, and collects the resets to send
/// for TCP segments that belong to nothing.
#[derive(Default)]
struct KernelSockets {
    resets: tcp::Resets,
}

impl Sockets for KernelSockets {
    fn tcp(&mut self, from: Ipv4Addr, to: Ipv4Addr, segment: &[u8]) -> bool {
        tcp::receive(from, to, segment, &mut self.resets)
    }

    fn udp(&mut self, from: Ipv4Addr, from_port: u16, port: u16, data: &[u8]) -> bool {
        let Some(socket) = socket_on(&SOCKETS.lock(), port) else {
            return false;
        };
        let mut copy = Vec::new();
        if copy.try_reserve_exact(data.len()).is_ok() {
            copy.extend_from_slice(data);
            let mut queue = socket.queue.lock();
            if queue.len() < SOCKET_QUEUE_MAX {
                queue.push_back(Received {
                    from,
                    from_port,
                    data: copy,
                });
            }
        }
        socket.readers.wake_all();
        poll::notify();
        // `socket` is dropped here, after the table's lock was released.
        true
    }
}

/// The interface as programs are told about it.
pub fn info() -> NetInfo {
    let mut info = NetInfo::default();
    let guard = INTERFACE.lock();
    let Some(interface) = guard.as_ref() else {
        return info;
    };
    info.flags = INFO_PRESENT;
    info.mac = interface.stack.mac().0;
    if let Some(config) = interface.stack.config() {
        info.flags |= INFO_CONFIGURED;
        info.address = config.address.0;
        info.mask = config.mask.0;
        if let Some(router) = config.router {
            info.flags |= INFO_ROUTER;
            info.router = router.0;
        }
        if let Some(dns) = config.dns {
            info.flags |= INFO_NAME_SERVER;
            info.name_server = dns.0;
        }
    }
    info
}
