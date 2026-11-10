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

use omit_inet::dns;
use omit_rt::abi::net::INFO_NAME_SERVER;
use omit_rt::net::LookupError;
use omit_rt::{eprintln, println};

omit_rt::entry!(main);

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
    match omit_rt::net::lookup_at(name, server, port) {
        Ok(address) => {
            println!("{name} has address {}", show(address));
            0
        }
        Err(LookupError::NoAnswer) => {
            eprintln!("host: {name}: no answer from {}", show(server));
            1
        }
        Err(error) => {
            eprintln!("host: {name}: {error}");
            1
        }
    }
}
