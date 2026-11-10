// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Fetches a file from a TFTP server.
//!
//!     tftp SERVER FILE [DESTINATION]
//!
//! SERVER is an IPv4 address. Without DESTINATION the file is stored
//! under its own name (without directories) in the current directory.

#![no_std]
#![no_main]

use omit_inet::tftp::{self, Packet, Step};
use omit_rt::abi::{Errno, open};
use omit_rt::net::UdpSocket;
use omit_rt::{eprintln, println};

omit_rt::entry!(main);

/// How long to wait for the server before repeating the last packet.
const TIMEOUT_MS: u64 = 1000;
/// How often the same packet is sent before giving up.
const ATTEMPTS: u32 = 5;

enum Failure {
    /// The server refused; its message has been printed.
    Refused,
    Network(&'static str),
    File(Errno),
}

/// Fetches `file` from `server` into the open file `out`; returns the
/// number of bytes.
fn fetch(server: [u8; 4], file: &str, out: u64) -> Result<u64, Failure> {
    let socket = UdpSocket::bind(0).map_err(|_| Failure::Network("cannot open a socket"))?;
    let mut last = [0u8; tftp::MAX_PACKET_LEN];
    let mut last_len = tftp::write_read_request(&mut last, file)
        .ok_or(Failure::Network("the file name is empty or too long"))?;
    // The request goes to the well-known port; the server answers from a
    // port it chooses for this transfer, and everything else goes there.
    let mut peer_port = tftp::PORT;
    let mut transfer_port = None;
    let mut expected: u16 = 1;
    let mut total = 0u64;
    let mut attempts = 0;
    loop {
        if attempts == ATTEMPTS {
            return Err(Failure::Network("the server stopped answering"));
        }
        attempts += 1;
        match socket.send_to(&last[..last_len], server, peer_port) {
            Ok(()) => {}
            Err(Errno::Unreachable) => {
                return Err(Failure::Network("the server cannot be reached"));
            }
            Err(_) => return Err(Failure::Network("cannot send")),
        }
        let deadline = omit_rt::uptime_ms() + TIMEOUT_MS;
        loop {
            let remaining = deadline.saturating_sub(omit_rt::uptime_ms());
            if remaining == 0 {
                break;
            }
            let mut packet = [0u8; tftp::MAX_PACKET_LEN + 1];
            let Ok((count, from, from_port)) = socket.receive_from(&mut packet, Some(remaining))
            else {
                break;
            };
            if from != server || transfer_port.is_some_and(|port| port != from_port) {
                continue;
            }
            match Packet::parse(&packet[..count]) {
                Some(Packet::Error { code, message }) => {
                    let text = core::str::from_utf8(message).unwrap_or("(unreadable message)");
                    eprintln!("tftp: {file}: the server says: {text} (error {code})");
                    return Err(Failure::Refused);
                }
                Some(Packet::Data { block, data }) => match tftp::step(expected, block, data.len())
                {
                    Step::Accept { last: finished } => {
                        transfer_port = Some(from_port);
                        peer_port = from_port;
                        omit_rt::write_all(out, data).map_err(Failure::File)?;
                        total += data.len() as u64;
                        last_len = tftp::write_ack(&mut last, block).unwrap_or(0);
                        if finished {
                            // The final acknowledgement is sent once; if it
                            // is lost the server gives up by itself.
                            let _ = socket.send_to(&last[..last_len], server, peer_port);
                            return Ok(total);
                        }
                        expected = expected.wrapping_add(1);
                        attempts = 0;
                        // Send the acknowledgement and wait for the next block.
                        break;
                    }
                    // The server repeated a block: our acknowledgement was
                    // lost or late. Send it again.
                    Step::Repeat => break,
                    Step::Ignore => {}
                },
                None => {}
            }
        }
    }
}

fn main() -> i32 {
    let mut args = omit_rt::args().skip(1);
    let (Some(server_text), Some(file), destination, None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        eprintln!("usage: tftp SERVER FILE [DESTINATION]");
        return 2;
    };
    let Some(server) = omit_inet::parse_ipv4(server_text) else {
        eprintln!("tftp: {server_text}: not an IPv4 address");
        return 2;
    };
    let destination = destination.unwrap_or_else(|| file.rsplit('/').next().unwrap_or(file));
    if destination.is_empty() {
        eprintln!("tftp: {file}: name a destination file");
        return 2;
    }
    let out = match omit_rt::open(destination, open::WRITE | open::CREATE | open::TRUNCATE) {
        Ok(fd) => fd,
        Err(error) => {
            eprintln!("tftp: {destination}: {error}");
            return 1;
        }
    };
    let outcome = fetch(server, file, out);
    let _ = omit_rt::close(out);
    match outcome {
        Ok(bytes) => {
            println!("{file}: {bytes} bytes from {server_text}, stored as {destination}");
            0
        }
        Err(failure) => {
            // Do not leave half a file behind.
            let _ = omit_rt::remove(destination);
            match failure {
                Failure::Refused => {}
                Failure::Network(reason) => eprintln!("tftp: {file}: {reason}"),
                Failure::File(error) => eprintln!("tftp: {destination}: {error}"),
            }
            1
        }
    }
}
