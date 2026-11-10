// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Fetches a file over HTTP.
//!
//!     fetch http://HOST[:PORT]/PATH [DESTINATION]
//!
//! HOST is a name or an IPv4 address. Without DESTINATION the body is
//! written to standard output. Only `http://`: there is no TLS.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use omit_inet::http::{self, HeadState, Url, UrlError};
use omit_rt::abi::{Errno, STDOUT, open};
use omit_rt::eprintln;
use omit_rt::net::TcpStream;

omit_rt::entry!(main);

/// How long to wait for the server to accept the connection.
const CONNECT_TIMEOUT_MS: u64 = 10_000;

fn fetch(url: &Url<'_>, address: [u8; 4], destination: Option<&str>) -> Result<u64, i32> {
    let stream =
        TcpStream::connect(address, url.port, Some(CONNECT_TIMEOUT_MS)).map_err(|error| {
            let reason = match error {
                Errno::Refused => "nothing listens on that port",
                Errno::Unreachable => "the host cannot be reached",
                Errno::TimedOut => "the host does not answer",
                _ => "cannot connect",
            };
            eprintln!("fetch: {}: {reason}", url.host);
            1
        })?;
    let mut request = [0u8; 2048];
    let Some(length) = http::write_get(&mut request, url) else {
        eprintln!("fetch: the address is too long");
        return Err(2);
    };
    let broken = |error: Errno| {
        eprintln!("fetch: {}: the connection failed: {error}", url.host);
        1
    };
    stream.write_all(&request[..length]).map_err(broken)?;

    // The head, and whatever of the body arrives with it.
    let mut received = Vec::new();
    let mut chunk = [0u8; 4096];
    let head = loop {
        match http::parse_head(&received) {
            HeadState::Complete(head) => break head,
            HeadState::Invalid => {
                eprintln!("fetch: {}: the answer is not HTTP", url.host);
                return Err(1);
            }
            HeadState::Incomplete => {}
        }
        match stream.read(&mut chunk).map_err(broken)? {
            0 => {
                eprintln!(
                    "fetch: {}: the server closed the connection without answering",
                    url.host
                );
                return Err(1);
            }
            count => received.extend_from_slice(&chunk[..count]),
        }
    };
    if head.status != 200 {
        match head.location {
            Some(location) if (300..400).contains(&head.status) => eprintln!(
                "fetch: the server says the file is at {location} ({} {}); redirects are not followed",
                head.status, head.reason
            ),
            _ => eprintln!("fetch: the server says: {} {}", head.status, head.reason),
        }
        return Err(1);
    }
    let expected = head.content_length;
    let body_start = head.length;

    let out = match destination {
        Some(path) => {
            omit_rt::open(path, open::WRITE | open::CREATE | open::TRUNCATE).map_err(|error| {
                eprintln!("fetch: {path}: {error}");
                1
            })?
        }
        None => STDOUT,
    };
    let finish = |outcome: Result<u64, i32>| {
        if let Some(path) = destination {
            let _ = omit_rt::close(out);
            if outcome.is_err() {
                // Do not leave half a file behind.
                let _ = omit_rt::remove(path);
            }
        }
        outcome
    };
    let store = |data: &[u8]| {
        omit_rt::write_all(out, data).map_err(|error| {
            eprintln!(
                "fetch: {}: {error}",
                destination.unwrap_or("standard output")
            );
            1
        })
    };
    let mut total = (received.len() - body_start) as u64;
    if let Err(status) = store(&received[body_start..]) {
        return finish(Err(status));
    }
    // The body ends where the server says, or where it closes.
    while expected.is_none_or(|expected| total < expected) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                if let Err(status) = store(&chunk[..count]) {
                    return finish(Err(status));
                }
                total += count as u64;
            }
            Err(error) => return finish(Err(broken(error))),
        }
    }
    if let Some(expected) = expected
        && total < expected
    {
        eprintln!("fetch: the connection ended after {total} of {expected} bytes");
        return finish(Err(1));
    }
    finish(Ok(total))
}

fn main() -> i32 {
    let mut args = omit_rt::args().skip(1);
    let (Some(text), destination, None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: fetch http://HOST[:PORT]/PATH [DESTINATION]");
        return 2;
    };
    let url = match Url::parse(text) {
        Ok(url) => url,
        Err(error) => {
            let reason = match error {
                UrlError::Scheme => "only http:// addresses can be fetched (there is no TLS)",
                UrlError::Host => "no host name in the address",
                UrlError::Port => "the port is not a number between 1 and 65535",
                UrlError::Character => "the address contains a space or a control character",
            };
            eprintln!("fetch: {text}: {reason}");
            return 2;
        }
    };
    let address = match omit_rt::net::lookup(url.host) {
        Ok(address) => address,
        Err(error) => {
            eprintln!("fetch: {}: {error}", url.host);
            return 1;
        }
    };
    match fetch(&url, address, destination) {
        Ok(bytes) => {
            if let Some(path) = destination {
                // (With the body on standard output, nothing else goes there.)
                omit_rt::println!("{text}: {bytes} bytes, stored as {path}");
            }
            0
        }
        Err(status) => status,
    }
}
