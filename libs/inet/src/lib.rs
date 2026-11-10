// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Internet protocols that programs speak over sockets, as opposed to
//! those the kernel implements: looking up names (DNS, RFC 1035) and
//! fetching files (TFTP, RFC 1350).
//!
//! Like the kernel's protocol library this is formats and decisions
//! only — pure functions over byte slices (one of them returns text it
//! puts together), tested on the build machine.
//! Sending, receiving and waiting are the program's business.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod dns;
pub mod http;
pub mod tftp;

/// Parses an IPv4 address in dotted-decimal notation (`10.0.2.2`):
/// exactly four decimal numbers of one to three digits, each at most 255.
pub fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = text.split('.');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    parts.next().is_none().then_some(octets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_decimal_is_parsed_strictly() {
        assert_eq!(parse_ipv4("10.0.2.2"), Some([10, 0, 2, 2]));
        assert_eq!(parse_ipv4("255.255.255.255"), Some([255; 4]));
        for bad in [
            "",
            "1.2.3",
            "1.2.3.4.5",
            "1.2.3.256",
            "1..2.3",
            "a.b.c.d",
            "1.2.3.4 ",
            "+1.2.3.4",
        ] {
            assert_eq!(parse_ipv4(bad), None, "{bad:?}");
        }
    }
}
