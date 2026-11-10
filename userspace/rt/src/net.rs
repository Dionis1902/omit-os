// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The network: what the interface is, and UDP sockets.

use omit_abi::net::{NO_TIMEOUT, NetInfo, SocketAddr};
use omit_abi::{Errno, nr};

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
