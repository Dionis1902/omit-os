// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! A TCP echo server: sends back whatever it receives.
//!
//!     echod PORT [CONNECTIONS]
//!
//! Serves connections one after another; with CONNECTIONS, ends after
//! that many (which is how the system's tests use it), otherwise runs
//! until killed.

#![no_std]
#![no_main]

use omit_rt::net::{TcpListener, TcpStream};
use omit_rt::{eprintln, println};

omit_rt::entry!(main);

/// Echoes until the other side stops sending; returns the byte count.
fn serve(stream: &TcpStream) -> Result<u64, omit_rt::abi::Errno> {
    let mut buffer = [0u8; 2048];
    let mut total = 0;
    loop {
        match stream.read(&mut buffer)? {
            0 => return Ok(total),
            count => {
                stream.write_all(&buffer[..count])?;
                total += count as u64;
            }
        }
    }
}

fn main() -> i32 {
    let mut args = omit_rt::args().skip(1);
    let port = args.next().and_then(|port| port.parse::<u16>().ok());
    // No count: no limit. A count that is not a positive number: wrong.
    let limit = match args.next() {
        None => Some(None),
        Some(count) => count.parse::<u32>().ok().filter(|&n| n > 0).map(Some),
    };
    let (Some(port @ 1..), Some(limit), None) = (port, limit, args.next()) else {
        eprintln!("usage: echod PORT [CONNECTIONS]");
        return 2;
    };
    let listener = match TcpListener::bind(port) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("echod: port {port}: {error}");
            return 1;
        }
    };
    println!("echod: listening on port {port}");
    let mut served = 0;
    while limit.is_none_or(|limit| served < limit) {
        let Ok((stream, [a, b, c, d], _)) = listener.accept(None) else {
            eprintln!("echod: cannot accept a connection");
            return 1;
        };
        match serve(&stream) {
            Ok(bytes) => println!("echod: echoed {bytes} bytes for {a}.{b}.{c}.{d}"),
            Err(error) => println!("echod: connection from {a}.{b}.{c}.{d} failed: {error}"),
        }
        served += 1;
    }
    0
}
