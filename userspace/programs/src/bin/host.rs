// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Looks up the IPv4 address of a name by asking a name server.
//!
//!     host NAME [SERVER[:PORT]]
//!
//! Without SERVER, the name server the network interface was told about
//! (by DHCP) is asked.

#![no_std]
#![no_main]

use omit_inet::dns::{self, ResponseError};
use omit_rt::abi::Errno;
use omit_rt::abi::net::INFO_NAME_SERVER;
use omit_rt::net::UdpSocket;
use omit_rt::{eprintln, println};

omit_rt::entry!(main);

/// How long to wait for an answer before asking again.
const TIMEOUT_MS: u64 = 2000;
const ATTEMPTS: u32 = 3;

fn show(address: [u8; 4]) -> impl core::fmt::Display {
    struct Dotted([u8; 4]);
    impl core::fmt::Display for Dotted {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let [a, b, c, d] = self.0;
            write!(f, "{a}.{b}.{c}.{d}")
        }
    }
    Dotted(address)
}

/// Asks `server` for the address of `name`. `Ok(None)`: no answer.
fn lookup(name: &str, server: [u8; 4], port: u16) -> Result<Option<[u8; 4]>, &'static str> {
    let mut query = [0u8; 300];
    // Answers are matched to questions by this number; the clock makes
    // it differ from one run to the next.
    let id = omit_rt::clock_us() as u16;
    let length = dns::write_query(&mut query, id, name).map_err(|error| match error {
        dns::NameError::Empty => "the name is empty or has an empty part",
        dns::NameError::TooLong => "the name is too long",
        dns::NameError::BadCharacter => "the name contains a character names cannot have",
    })?;
    let socket = UdpSocket::bind(0).map_err(|_| "cannot open a socket")?;
    for _ in 0..ATTEMPTS {
        match socket.send_to(&query[..length], server, port) {
            Ok(()) => {}
            Err(Errno::Unreachable) => return Err("the name server cannot be reached"),
            Err(_) => return Err("cannot send the question"),
        }
        let deadline = omit_rt::uptime_ms() + TIMEOUT_MS;
        loop {
            let remaining = deadline.saturating_sub(omit_rt::uptime_ms());
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
                Ok(address) => return Ok(Some(address)),
                // Not an answer to this question: keep waiting.
                Err(ResponseError::Malformed) => {}
                Err(ResponseError::NoSuchName) => return Err("no such name"),
                Err(ResponseError::NoAddress) => return Err("the name has no IPv4 address"),
                Err(ResponseError::Truncated) => {
                    return Err("the answer is too long for this program");
                }
                Err(ResponseError::ServerFailure(_)) => {
                    return Err("the name server reports a failure");
                }
            }
        }
    }
    Ok(None)
}

fn main() -> i32 {
    let mut args = omit_rt::args().skip(1);
    let (Some(name), server, None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: host NAME [SERVER[:PORT]]");
        return 2;
    };
    if let Some(address) = omit_inet::parse_ipv4(name) {
        println!("{} is an address already", show(address));
        return 0;
    }
    let (server, port) = match server {
        Some(text) => {
            let (address, port) = match text.split_once(':') {
                Some((address, port)) => (address, port.parse().ok()),
                None => (text, Some(dns::PORT)),
            };
            let (Some(address), Some(port)) = (omit_inet::parse_ipv4(address), port) else {
                eprintln!("host: {text}: not an IPv4 address, or address:port");
                return 2;
            };
            (address, port)
        }
        None => {
            let info = omit_rt::net::info();
            if info.flags & INFO_NAME_SERVER == 0 {
                eprintln!("host: no name server is known; name one: host NAME SERVER");
                return 1;
            }
            (info.name_server, dns::PORT)
        }
    };
    match lookup(name, server, port) {
        Ok(Some(address)) => {
            println!("{name} has address {}", show(address));
            0
        }
        Ok(None) => {
            eprintln!("host: {name}: no answer from {}", show(server));
            1
        }
        Err(reason) => {
            eprintln!("host: {name}: {reason}");
            1
        }
    }
}
