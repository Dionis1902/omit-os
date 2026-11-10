// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Fetches a file over HTTP, plain or encrypted.
//!
//!     fetch [--trust FILE] http[s]://HOST[:PORT]/PATH [DESTINATION]
//!
//! HOST is a name or an IPv4 address. Without DESTINATION the body is
//! written to standard output. Where the server says the file is
//! elsewhere, that is followed, up to five times — but never from an
//! `https://` address to an `http://` one.
//!
//! An `https://` address is fetched over TLS 1.3: the server must show
//! a certificate for HOST that leads to an authority in the built-in
//! list (Mozilla's) or to the one in FILE (a certificate, PEM or DER).
//! See the `omit-tls` crate for what that protection rests on.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use omit_inet::http::{self, HeadState, Url, UrlError};
use omit_rt::abi::{Errno, STDOUT, open};
use omit_rt::eprintln;
use omit_rt::net::TcpStream;

omit_rt::entry!(main);

/// How long to wait for the server to accept the connection.
const CONNECT_TIMEOUT_MS: u64 = 10_000;

/// A TCP connection as what TLS runs over.
struct Tcp(TcpStream);

impl omit_tls::Transport for Tcp {
    type Error = Errno;

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Errno> {
        self.0.read(buffer)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), Errno> {
        self.0.write_all(data)
    }
}

/// The system's clock, for judging certificates.
#[derive(Debug)]
struct System;

impl omit_tls::Platform for System {
    fn now(&self) -> Option<u64> {
        omit_rt::realtime_us().map(|micros| micros / 1_000_000)
    }
}

/// Set when random numbers were handed out that rest on timing alone.
static WEAK_RANDOM: AtomicBool = AtomicBool::new(false);

fn random(buffer: &mut [u8]) -> bool {
    if !omit_rt::system::random(buffer) {
        WEAK_RANDOM.store(true, Ordering::Relaxed);
    }
    true
}

/// Why reading or writing failed.
enum Broken {
    Plain(Errno),
    Secure(omit_tls::Error<Errno>),
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plain(error) => write!(f, "the connection failed: {error}"),
            Self::Secure(error) => write!(f, "{error}"),
        }
    }
}

/// The connection to the server, encrypted or not.
enum Link {
    Plain(TcpStream),
    Secure(Box<omit_tls::Stream<Tcp>>),
}

impl Link {
    fn write_all(&mut self, data: &[u8]) -> Result<(), Broken> {
        match self {
            Self::Plain(stream) => stream.write_all(data).map_err(Broken::Plain),
            Self::Secure(stream) => stream.write_all(data).map_err(Broken::Secure),
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Broken> {
        match self {
            Self::Plain(stream) => stream.read(buffer).map_err(Broken::Plain),
            Self::Secure(stream) => stream.read(buffer).map_err(Broken::Secure),
        }
    }
}

/// How often one request may be sent elsewhere.
const MAX_REDIRECTS: usize = 5;

/// How a request ended, when it did not fail.
enum Fetched {
    /// The body arrived: this many bytes.
    Body(u64),
    /// The server says the file is at this address instead.
    Elsewhere(String),
}

fn fetch(
    url: &Url<'_>,
    address: [u8; 4],
    trusted: &[Vec<u8>],
    destination: Option<&str>,
) -> Result<Fetched, i32> {
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
    let mut stream = if url.secure {
        omit_tls::set_random_source(random);
        let secure = omit_tls::Stream::connect(Tcp(stream), url.host, trusted, System);
        if WEAK_RANDOM.load(Ordering::Relaxed) {
            eprintln!(
                "fetch: warning: this processor offers no random numbers; the connection's \
                 secrets rest on timing and may be guessable"
            );
        }
        match secure {
            Ok(stream) => Link::Secure(Box::new(stream)),
            Err(error) => {
                eprintln!("fetch: {}: {error}", url.host);
                return Err(1);
            }
        }
    } else {
        Link::Plain(stream)
    };
    let mut request = [0u8; 2048];
    let Some(length) = http::write_get(&mut request, url) else {
        eprintln!("fetch: the address is too long");
        return Err(2);
    };
    let broken = |error: Broken| {
        eprintln!("fetch: {}: {error}", url.host);
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
        let elsewhere = head
            .location
            .filter(|_| (300..400).contains(&head.status))
            .map(|location| (location, http::redirect_target(url, location)));
        match elsewhere {
            Some((_, Some(target))) => return Ok(Fetched::Elsewhere(target)),
            Some((location, None)) => eprintln!(
                "fetch: the server says the file is at {location} ({} {}), which cannot be fetched",
                head.status, head.reason
            ),
            None => eprintln!("fetch: the server says: {} {}", head.status, head.reason),
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
    let finish = |outcome: Result<u64, i32>| -> Result<Fetched, i32> {
        if let Some(path) = destination {
            let _ = omit_rt::close(out);
            if outcome.is_err() {
                // Do not leave half a file behind.
                let _ = omit_rt::remove(path);
            }
        }
        outcome.map(Fetched::Body)
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
    // The body ends where the server says, or where it closes. (An
    // encrypted connection that just stops, without the server saying
    // it has finished, is an error unless the stated length is there:
    // otherwise a body cut short could not be told from a whole one.)
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
    const USAGE: &str = "usage: fetch [--trust FILE] http[s]://HOST[:PORT]/PATH [DESTINATION]";
    let mut args = omit_rt::args().skip(1).peekable();
    let mut trusted = Vec::new();
    while args.peek() == Some(&"--trust") {
        args.next();
        let Some(file) = args.next() else {
            eprintln!("{USAGE}");
            return 2;
        };
        let read = omit_rt::read_file(file).map_err(|error| alloc::format!("{error}"));
        match read.map(|data| omit_tls::certificates(&data)) {
            Ok(Some(found)) => trusted.extend(found),
            Ok(None) => {
                eprintln!("fetch: {file}: not a certificate (PEM or DER)");
                return 2;
            }
            Err(reason) => {
                eprintln!("fetch: {file}: {reason}");
                return 2;
            }
        }
    }
    let (Some(text), destination, None) = (args.next(), args.next(), args.next()) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let mut current = String::from(text);
    for hop in 0..=MAX_REDIRECTS {
        let url = match Url::parse(&current) {
            Ok(url) => url,
            Err(error) => {
                let reason = match error {
                    UrlError::Scheme => "only http:// and https:// addresses can be fetched",
                    UrlError::Host => "no host name in the address",
                    UrlError::Port => "the port is not a number between 1 and 65535",
                    UrlError::Character => "the address contains a space or a control character",
                };
                eprintln!("fetch: {current}: {reason}");
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
        let next = match fetch(&url, address, &trusted, destination) {
            Ok(Fetched::Body(bytes)) => {
                if let Some(path) = destination {
                    // (With the body on standard output, nothing else goes there.)
                    omit_rt::println!("{current}: {bytes} bytes, stored as {path}");
                }
                return 0;
            }
            Ok(Fetched::Elsewhere(next)) => next,
            Err(status) => return status,
        };
        if hop == MAX_REDIRECTS {
            eprintln!("fetch: sent elsewhere more than {MAX_REDIRECTS} times; giving up at {next}");
            return 1;
        }
        // What was asked for privately is not then fetched in the open.
        let open = !next
            .get(..8)
            .is_some_and(|start| start.eq_ignore_ascii_case("https://"));
        if url.secure && open {
            eprintln!(
                "fetch: the server sends an https:// request on to {next}, which is not followed"
            );
            return 1;
        }
        eprintln!("fetch: the file is at {next}");
        current = next;
    }
    1
}
