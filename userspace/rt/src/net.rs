// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The network: what the interface is, UDP sockets, TCP connections and
//! listeners, and looking names up.

use omit_abi::net::{NO_TIMEOUT, NetInfo, SocketAddr};
use omit_abi::{Errno, nr};
use omit_inet::dns::{self, ResponseError};

use crate::{result, syscall, syscall4, syscall5};

/// Describes the machine's network interface; see the flags in
/// [`omit_abi::net`].
pub fn info() -> NetInfo {
    let mut info = NetInfo::default();
    // SAFETY: the pointer refers to a live, writable `NetInfo`.
    unsafe { syscall(nr::NET_INFO, &raw mut info as u64, 0, 0) };
    info
}

/// A UDP socket: datagrams are sent from, and received on, its port.
/// Closed when dropped.
#[derive(Debug)]
pub struct UdpSocket {
    fd: u64,
}

impl UdpSocket {
    /// Opens a socket on local port `port`, or on a free port if `port`
    /// is 0. Fails with `Busy` if the port is taken.
    pub fn bind(port: u16) -> Result<Self, Errno> {
        // SAFETY: plain integer argument.
        result(unsafe { syscall(nr::UDP_BIND, u64::from(port), 0, 0) }).map(|fd| Self { fd })
    }

    /// The descriptor, for [`crate::poll`].
    pub fn fd(&self) -> u64 {
        self.fd
    }

    /// Sends `data` (at most [`omit_abi::net::UDP_PAYLOAD_MAX`] bytes)
    /// as one datagram to `address`, port `port`. Fails with
    /// `Unreachable` if there is no way to send it there.
    pub fn send_to(&self, data: &[u8], address: [u8; 4], port: u16) -> Result<(), Errno> {
        let to = SocketAddr {
            address,
            port,
            reserved: 0,
        };
        // SAFETY: the pointers refer to a live `SocketAddr` and a live,
        // readable slice of the given length.
        result(unsafe {
            syscall4(
                nr::UDP_SEND,
                self.fd,
                &raw const to as u64,
                data.as_ptr() as u64,
                data.len() as u64,
            )
        })
        .map(|_| ())
    }

    /// Waits for a datagram — for at most `timeout_ms` milliseconds, or
    /// for ever with `None` — and copies it into `buffer`, cutting it
    /// short if it is longer. Returns the number of bytes and the
    /// sender's address and port; `TimedOut` if nothing arrived
    /// (`WouldBlock` with a timeout of 0).
    pub fn receive_from(
        &self,
        buffer: &mut [u8],
        timeout_ms: Option<u64>,
    ) -> Result<(usize, [u8; 4], u16), Errno> {
        let mut from = SocketAddr::default();
        // SAFETY: the pointers refer to a live, writable slice of the
        // given length and a live, writable `SocketAddr`.
        let count = result(unsafe {
            syscall5(
                nr::UDP_RECEIVE,
                self.fd,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                &raw mut from as u64,
                timeout_ms.unwrap_or(NO_TIMEOUT),
            )
        })?;
        Ok((count as usize, from.address, from.port))
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        let _ = crate::close(self.fd);
    }
}

/// Why a name could not be turned into an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupError {
    /// The name is not one a name server can be asked about.
    BadName,
    /// The interface knows no name server.
    NoNameServer,
    /// The name server cannot be reached or did not answer.
    NoAnswer,
    /// The name server says there is no such name.
    NoSuchName,
    /// The name exists but has no IPv4 address.
    NoAddress,
    /// The name server reported a failure, or its answer was unusable.
    ServerFailure,
}

impl core::fmt::Display for LookupError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::BadName => "not a name that can be looked up",
            Self::NoNameServer => "no name server is known",
            Self::NoAnswer => "the name server does not answer",
            Self::NoSuchName => "no such name",
            Self::NoAddress => "the name has no IPv4 address",
            Self::ServerFailure => "the name server reports a failure",
        })
    }
}

/// How long [`lookup_at`] waits for an answer before asking again.
const LOOKUP_TIMEOUT_MS: u64 = 2000;
const LOOKUP_ATTEMPTS: u32 = 3;

/// The IPv4 address of `host`: itself if it is written as an address,
/// otherwise what the interface's name server says.
pub fn lookup(host: &str) -> Result<[u8; 4], LookupError> {
    if let Some(address) = omit_inet::parse_ipv4(host) {
        return Ok(address);
    }
    let info = info();
    if info.flags & omit_abi::net::INFO_NAME_SERVER == 0 {
        return Err(LookupError::NoNameServer);
    }
    lookup_at(host, info.name_server, dns::PORT)
}

/// Asks the name server at `server`, port `port`, for the IPv4 address
/// of `name`.
pub fn lookup_at(name: &str, server: [u8; 4], port: u16) -> Result<[u8; 4], LookupError> {
    let mut query = [0u8; 300];
    // Answers are matched to questions by this number; the clock makes
    // it differ from one question to the next.
    let id = crate::clock_us() as u16;
    let length = dns::write_query(&mut query, id, name).map_err(|_| LookupError::BadName)?;
    let socket = UdpSocket::bind(0).map_err(|_| LookupError::NoAnswer)?;
    for _ in 0..LOOKUP_ATTEMPTS {
        socket
            .send_to(&query[..length], server, port)
            .map_err(|_| LookupError::NoAnswer)?;
        let deadline = crate::uptime_ms() + LOOKUP_TIMEOUT_MS;
        loop {
            let remaining = deadline.saturating_sub(crate::uptime_ms());
            if remaining == 0 {
                break;
            }
            let mut answer = [0u8; dns::MAX_MESSAGE_LEN];
            let Ok((count, from, from_port)) = socket.receive_from(&mut answer, Some(remaining))
            else {
                break;
            };
            if from != server || from_port != port {
                continue;
            }
            match dns::parse_response(&answer[..count], id) {
                Ok(address) => return Ok(address),
                // Not an answer to this question: keep waiting.
                Err(ResponseError::Malformed) => {}
                Err(ResponseError::NoSuchName) => return Err(LookupError::NoSuchName),
                Err(ResponseError::NoAddress) => return Err(LookupError::NoAddress),
                Err(ResponseError::Truncated | ResponseError::ServerFailure(_)) => {
                    return Err(LookupError::ServerFailure);
                }
            }
        }
    }
    Err(LookupError::NoAnswer)
}

/// A TCP connection: a stream of bytes in each direction. Closed when
/// dropped; what was written is still delivered.
#[derive(Debug)]
pub struct TcpStream {
    fd: u64,
}

impl TcpStream {
    /// Connects to `address`, port `port`, waiting up to `timeout_ms`
    /// milliseconds for the other side to answer (`None`: about a
    /// minute, after which the system gives up by itself). Fails with
    /// `Refused` if nothing listens there, `Unreachable` if there is no
    /// way to the address, `TimedOut` if nobody answers.
    pub fn connect(address: [u8; 4], port: u16, timeout_ms: Option<u64>) -> Result<Self, Errno> {
        let to = SocketAddr {
            address,
            port,
            reserved: 0,
        };
        // SAFETY: the pointer refers to a live `SocketAddr`.
        result(unsafe {
            syscall(
                nr::TCP_CONNECT,
                &raw const to as u64,
                timeout_ms.unwrap_or(NO_TIMEOUT),
                0,
            )
        })
        .map(|fd| Self { fd })
    }

    /// The descriptor, for [`crate::poll`], [`crate::read`] and
    /// [`crate::write`].
    pub fn fd(&self) -> u64 {
        self.fd
    }

    /// Reads what has arrived, waiting for at least one byte. `Ok(0)`
    /// means the other side has finished sending.
    pub fn read(&self, buffer: &mut [u8]) -> Result<usize, Errno> {
        crate::read(self.fd, buffer)
    }

    /// Sends all of `data`, waiting while the other side is slow to
    /// take it.
    pub fn write_all(&self, data: &[u8]) -> Result<(), Errno> {
        crate::write_all(self.fd, data)
    }

    /// Tells the other side that nothing more will be sent. Reading
    /// goes on.
    pub fn shutdown(&self) -> Result<(), Errno> {
        // SAFETY: plain integer argument.
        result(unsafe { syscall(nr::TCP_SHUTDOWN, self.fd, 0, 0) }).map(|_| ())
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        let _ = crate::close(self.fd);
    }
}

/// A TCP port on which connections are accepted. Stops listening when
/// dropped.
#[derive(Debug)]
pub struct TcpListener {
    fd: u64,
}

impl TcpListener {
    /// Starts listening on `port`. Fails with `Busy` if somebody
    /// already does.
    pub fn bind(port: u16) -> Result<Self, Errno> {
        // SAFETY: plain integer argument.
        result(unsafe { syscall(nr::TCP_LISTEN, u64::from(port), 0, 0) }).map(|fd| Self { fd })
    }

    /// The descriptor, for [`crate::poll`]: readable when a connection
    /// is waiting.
    pub fn fd(&self) -> u64 {
        self.fd
    }

    /// Waits for a connection — for at most `timeout_ms` milliseconds,
    /// or for ever with `None` — and returns it with the address and
    /// port it comes from. `TimedOut` if none arrived (`WouldBlock`
    /// with a timeout of 0).
    pub fn accept(&self, timeout_ms: Option<u64>) -> Result<(TcpStream, [u8; 4], u16), Errno> {
        let mut from = SocketAddr::default();
        // SAFETY: the pointer refers to a live, writable `SocketAddr`.
        let fd = result(unsafe {
            syscall(
                nr::TCP_ACCEPT,
                self.fd,
                &raw mut from as u64,
                timeout_ms.unwrap_or(NO_TIMEOUT),
            )
        })?;
        Ok((TcpStream { fd }, from.address, from.port))
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        let _ = crate::close(self.fd);
    }
}
