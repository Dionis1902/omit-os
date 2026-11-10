// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! TCP connections and listeners for programs.
//!
//! The protocol itself is `omit_net::tcp`; this module keeps the
//! connections, feeds them the segments that arrive, sends what they
//! want sent, and lets threads wait on them.
//!
//! All sending happens in one place, [`pump`], which the interface calls
//! whenever it has looked for received frames: from the receive thread a
//! hundred times a second, and from any thread that has just done
//! something to a connection (see `kick`). Retransmission timers are
//! therefore accurate to a timer tick.
//!
//! A connection lives here until the protocol has finished with it, not
//! until the program closes its descriptor: what was written is still
//! delivered after a close. A connection nobody holds any more is given
//! [`ORPHAN_MS`] to finish and is then reset.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use omit_net::Ipv4Addr;
use omit_net::stack::{Link, Stack};
use omit_net::tcp::{self, Connection, Segment, State};

use super::{Waited, kick, now_ms, wait_until};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::{poll, time};

/// Connections a listener holds for a program that has not accepted
/// them yet; further ones are refused.
const BACKLOG_MAX: usize = 8;
/// How long a connection whose descriptor is closed may take to finish.
const ORPHAN_MS: u64 = 30_000;
/// Ports for outgoing connections.
const EPHEMERAL_PORTS: core::ops::RangeInclusive<u16> = 49152..=65535;

/// Why a TCP operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpError {
    /// The port is being listened on already, or no port is free.
    PortInUse,
    /// No interface, no address, no route, or nobody answers for the
    /// next hop.
    Unreachable,
    /// Nobody listens on the port at the other end.
    Refused,
    /// The peer reset the connection.
    Reset,
    /// The peer did not answer, or the time allowed has passed.
    TimedOut,
    /// Nothing can be done right now and the caller will not wait.
    WouldBlock,
    /// Our side of the connection is closed: nothing more can be sent.
    Closed,
    /// The waiting thread's process is being terminated.
    Killed,
}

impl From<tcp::Error> for TcpError {
    fn from(error: tcp::Error) -> Self {
        match error {
            tcp::Error::Refused => Self::Refused,
            tcp::Error::Reset => Self::Reset,
            tcp::Error::TimedOut => Self::TimedOut,
        }
    }
}

/// One connection.
pub struct TcpSocket {
    connection: SpinLock<Connection>,
    /// Woken whenever the connection may have changed.
    changed: WaitQueue,
    /// When the last descriptor was closed (milliseconds; 0: not yet).
    orphaned_at: AtomicU64,
}

/// Every connection the protocol has not finished with.
static CONNECTIONS: SpinLock<Vec<Arc<TcpSocket>>> = SpinLock::new(Vec::new());
/// Ports being listened on. The port is kept beside the reference so
/// that a listener is only ever referenced when it is the one wanted.
static LISTENERS: SpinLock<Vec<(u16, Weak<TcpListener>)>> = SpinLock::new(Vec::new());
static NEXT_EPHEMERAL: AtomicU32 = AtomicU32::new(*EPHEMERAL_PORTS.start() as u32);
static SEQUENCE_STEP: AtomicU32 = AtomicU32::new(0);

/// An initial sequence number: the clock in 4 µs units, as the RFC
/// suggests, plus an increment per connection. It varies between
/// connections; it is not unpredictable to an attacker, which would
/// need a secret and a hash this kernel does not have yet.
fn initial_sequence() -> u32 {
    ((time::monotonic_ns() / 4000) as u32)
        .wrapping_add(SEQUENCE_STEP.fetch_add(0x9E37_79B9, Ordering::Relaxed))
}

impl TcpSocket {
    fn new(connection: Connection) -> Arc<Self> {
        let socket = Arc::new(Self {
            connection: SpinLock::new(connection),
            changed: WaitQueue::new(),
            orphaned_at: AtomicU64::new(0),
        });
        CONNECTIONS.lock().push(socket.clone());
        socket
    }

    /// Opens a connection to `remote`, port `port`, waiting up to
    /// `timeout_ticks` timer ticks (`None`: as long as the protocol
    /// itself keeps trying) for the peer to answer.
    pub fn connect(
        remote: Ipv4Addr,
        port: u16,
        timeout_ticks: Option<u64>,
    ) -> Result<Arc<Self>, TcpError> {
        // Find the next hop first: "nobody there" is known within a
        // second, where an unanswered SYN takes much longer.
        super::wait_resolved(remote).map_err(|killed| {
            if killed {
                TcpError::Killed
            } else {
                TcpError::Unreachable
            }
        })?;
        let local_port = {
            let connections = CONNECTIONS.lock();
            let in_use = |candidate: u16| {
                connections
                    .iter()
                    .any(|socket| socket.connection.lock().local_port() == candidate)
            };
            (0..EPHEMERAL_PORTS.len())
                .map(|_| {
                    let next = NEXT_EPHEMERAL.fetch_add(1, Ordering::Relaxed);
                    if next >= u32::from(*EPHEMERAL_PORTS.end()) {
                        NEXT_EPHEMERAL
                            .store(u32::from(*EPHEMERAL_PORTS.start()), Ordering::Relaxed);
                    }
                    (next as u16).max(*EPHEMERAL_PORTS.start())
                })
                .find(|&candidate| !in_use(candidate))
                .ok_or(TcpError::PortInUse)?
        };
        let socket = Self::new(Connection::connect(
            local_port,
            remote,
            port,
            initial_sequence(),
        ));
        kick();
        let outcome = wait_until(&socket.changed, timeout_ticks, || {
            let connection = socket.connection.lock();
            connection.is_open() || connection.state() == State::Closed
        });
        let error = match outcome {
            Waited::Ready => {
                let connection = socket.connection.lock();
                match connection.error() {
                    None if connection.is_open() => None,
                    Some(error) => Some(error.into()),
                    None => Some(TcpError::Reset),
                }
            }
            Waited::TimedOut => Some(TcpError::TimedOut),
            Waited::Killed => Some(TcpError::Killed),
        };
        match error {
            None => Ok(socket),
            Some(error) => {
                socket.connection.lock().abort();
                Err(error)
            }
        }
    }

    /// The peer's address and port.
    pub fn remote(&self) -> (Ipv4Addr, u16) {
        self.connection.lock().remote()
    }

    /// Whether a read would not wait: data, the end of the data, or an
    /// error is there to be had.
    pub fn ready(&self) -> bool {
        let connection = self.connection.lock();
        connection.readable() > 0 || connection.at_end()
    }

    /// Takes received data, waiting for some unless `wait` is unset.
    /// `Ok(0)` is the end: the peer has closed its side.
    pub fn read(&self, out: &mut [u8], wait: bool) -> Result<usize, TcpError> {
        loop {
            {
                let mut connection = self.connection.lock();
                let count = connection.read(out);
                if count > 0 || out.is_empty() {
                    drop(connection);
                    // Reading may have reopened the window: tell the peer.
                    kick();
                    return Ok(count);
                }
                if connection.at_end() {
                    return connection.error().map_or(Ok(0), |error| Err(error.into()));
                }
            }
            if !wait {
                return Err(TcpError::WouldBlock);
            }
            if let Waited::Killed = wait_until(&self.changed, None, || self.ready()) {
                return Err(TcpError::Killed);
            }
        }
    }

    /// Queues `data` for sending. With `wait`, returns when all of it
    /// has been taken; without, takes what fits now.
    pub fn write(&self, data: &[u8], wait: bool) -> Result<usize, TcpError> {
        let mut done = 0;
        loop {
            let open = {
                let mut connection = self.connection.lock();
                done += connection.write(&data[done..]);
                matches!(connection.state(), State::Established | State::CloseWait)
                    .then_some(())
                    .ok_or_else(|| connection.error().map_or(TcpError::Closed, Into::into))
            };
            kick();
            if done == data.len() {
                return Ok(done);
            }
            open?;
            if !wait {
                return if done > 0 {
                    Ok(done)
                } else {
                    Err(TcpError::WouldBlock)
                };
            }
            let writable = || {
                let connection = self.connection.lock();
                connection.writable() > 0
                    || !matches!(connection.state(), State::Established | State::CloseWait)
            };
            if let Waited::Killed = wait_until(&self.changed, None, writable) {
                return Err(TcpError::Killed);
            }
        }
    }

    /// Closes our side: what was written is still delivered, then the
    /// peer is told that nothing more follows. Receiving goes on.
    pub fn shutdown(&self) {
        self.connection.lock().close();
    }
}

/// What a descriptor refers to: the connection is closed when the last
/// reference to this goes.
pub struct TcpHandle(pub Arc<TcpSocket>);

impl Drop for TcpHandle {
    fn drop(&mut self) {
        // Only marks: this may run where sleeping is not possible. The
        // receive thread does the sending within a tick.
        self.0.connection.lock().close();
        self.0.orphaned_at.store(now_ms().max(1), Ordering::Relaxed);
    }
}

/// A port being listened on.
pub struct TcpListener {
    backlog: SpinLock<VecDeque<Arc<TcpSocket>>>,
    arrivals: WaitQueue,
}

impl TcpListener {
    /// Starts listening on `port`.
    pub fn bind(port: u16) -> Result<Arc<Self>, TcpError> {
        if port == 0 {
            return Err(TcpError::PortInUse);
        }
        let listener = Arc::new(Self {
            backlog: SpinLock::new(VecDeque::new()),
            arrivals: WaitQueue::new(),
        });
        let mut listeners = LISTENERS.lock();
        if listeners
            .iter()
            .any(|(taken, entry)| *taken == port && entry.strong_count() > 0)
        {
            drop(listeners);
            return Err(TcpError::PortInUse);
        }
        listeners.push((port, Arc::downgrade(&listener)));
        Ok(listener)
    }

    /// The first connection whose handshake has completed, if any.
    fn take(&self) -> Option<Arc<TcpSocket>> {
        let mut backlog = self.backlog.lock();
        // Connections that failed before anybody accepted them go.
        backlog.retain(|socket| socket.connection.lock().state() != State::Closed);
        let index = backlog
            .iter()
            .position(|socket| socket.connection.lock().is_open())?;
        backlog.remove(index)
    }

    /// Whether [`TcpListener::accept`] would not wait.
    pub fn ready(&self) -> bool {
        self.backlog
            .lock()
            .iter()
            .any(|socket| socket.connection.lock().is_open())
    }

    /// Takes the next connection made to the port, waiting up to
    /// `timeout_ticks` timer ticks (`None`: for ever).
    pub fn accept(&self, timeout_ticks: Option<u64>) -> Result<Arc<TcpSocket>, TcpError> {
        loop {
            if let Some(socket) = self.take() {
                return Ok(socket);
            }
            if timeout_ticks == Some(0) {
                return Err(TcpError::WouldBlock);
            }
            match wait_until(&self.arrivals, timeout_ticks, || self.ready()) {
                Waited::Ready => {}
                Waited::TimedOut => return Err(TcpError::TimedOut),
                Waited::Killed => return Err(TcpError::Killed),
            }
        }
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        // This listener's own entry can no longer be referenced.
        LISTENERS
            .lock()
            .retain(|(_, entry)| entry.strong_count() > 0);
        // Connections nobody will accept are reset.
        for socket in self.backlog.lock().drain(..) {
            socket.connection.lock().abort();
        }
    }
}

/// Resets to send for segments that belonged to nothing.
#[derive(Default)]
pub struct Resets(Vec<(Ipv4Addr, [u8; tcp::HEADER_LEN])>);

/// Takes in a TCP segment that arrived from `from` for our address
/// `to`. Returns whether it belonged to a connection or opened one.
pub fn receive(from: Ipv4Addr, to: Ipv4Addr, data: &[u8], resets: &mut Resets) -> bool {
    let Some(segment) = Segment::parse(data, from, to) else {
        return false;
    };
    let socket = CONNECTIONS
        .lock()
        .iter()
        .find(|socket| socket.connection.lock().matches(&segment, from))
        .cloned();
    if let Some(socket) = socket {
        socket.connection.lock().on_segment(&segment, now_ms());
        socket.changed.wake_all();
        // A connection waiting to be accepted may just have opened.
        wake_listener(segment.destination_port);
        poll::notify();
        return true;
    }
    let listener = LISTENERS
        .lock()
        .iter()
        .find(|(port, _)| *port == segment.destination_port)
        .and_then(|(_, entry)| entry.upgrade());
    if let Some(listener) = listener
        && listener.backlog.lock().len() < BACKLOG_MAX
        && let Some(connection) = Connection::accept(&segment, from, initial_sequence())
    {
        let socket = TcpSocket::new(connection);
        listener.backlog.lock().push_back(socket);
        return true;
    }
    if let Some(reset) = segment.reset_reply() {
        let mut bytes = [0u8; tcp::HEADER_LEN];
        if reset.write(&mut bytes, to, from).is_some() && resets.0.len() < 16 {
            resets.0.push((from, bytes));
        }
    }
    false
}

fn wake_listener(port: u16) {
    let listener = LISTENERS
        .lock()
        .iter()
        .find(|(taken, _)| *taken == port)
        .and_then(|(_, entry)| entry.upgrade());
    if let Some(listener) = listener {
        listener.arrivals.wake_all();
    }
}

/// Sends what the connections have to send — and the resets collected
/// while receiving — and forgets connections that have ended.
pub fn pump(stack: &mut Stack, link: &mut dyn Link, resets: Resets) {
    for (to, bytes) in resets.0 {
        let _ = stack.send_tcp(to, &bytes, link);
    }
    let sockets = CONNECTIONS.lock().clone();
    if sockets.is_empty() {
        return;
    }
    let Some(config) = stack.config() else {
        return;
    };
    let now = now_ms();
    let mut buffer = vec![0u8; tcp::HEADER_LEN + 4 + usize::from(tcp::MSS)];
    let mut ended = false;
    for socket in &sockets {
        let (remote, state_before) = {
            let mut connection = socket.connection.lock();
            let orphaned_at = socket.orphaned_at.load(Ordering::Relaxed);
            if orphaned_at != 0 && now.saturating_sub(orphaned_at) > ORPHAN_MS {
                connection.abort();
            }
            (connection.remote().0, connection.state())
        };
        if state_before == State::Closed {
            // Ended while receiving (the last acknowledgement, a reset).
            ended = true;
            continue;
        }
        // Without the next hop's hardware address nothing can go; the
        // request for it has just been (re)sent.
        if stack.resolve(remote, link).is_err() {
            continue;
        }
        loop {
            let length = socket
                .connection
                .lock()
                .poll_transmit(config.address, now, &mut buffer);
            let Some(length) = length else { break };
            let _ = stack.send_tcp(remote, &buffer[..length], link);
        }
        let state = socket.connection.lock().state();
        if state != state_before {
            socket.changed.wake_all();
            wake_listener(socket.connection.lock().local_port());
            poll::notify();
            ended |= state == State::Closed;
        }
    }
    if ended {
        CONNECTIONS
            .lock()
            .retain(|socket| socket.connection.lock().state() != State::Closed);
    }
}
