// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The question "what is the IPv4 address of this name?" and its answer
//! (RFC 1035): one query for an `A` record, sent to a recursive name
//! server over UDP.
//!
//! Not handled: answers too long for UDP (the truncation flag is
//! reported as an error rather than retried over TCP), IPv6 addresses,
//! any record type other than `A`.

/// UDP port name servers listen on.
pub const PORT: u16 = 53;
/// Longest message a server sends over UDP without extensions.
pub const MAX_MESSAGE_LEN: usize = 512;

const HEADER_LEN: usize = 12;
const FLAG_RESPONSE: u16 = 0x8000;
const FLAG_TRUNCATED: u16 = 0x0200;
const FLAG_RECURSION_DESIRED: u16 = 0x0100;
const OPCODE_MASK: u16 = 0x7800;
const RCODE_MASK: u16 = 0x000F;
const RCODE_NAME_ERROR: u16 = 3;
const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;

/// Why a name could not be turned into a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    /// Empty, or with an empty part (`a..b`).
    Empty,
    /// A part longer than 63 characters or the whole longer than 253.
    TooLong,
    /// A character other than a letter, digit, `-` or `_`.
    BadCharacter,
}

/// Why a response gave no address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseError {
    /// Not a well-formed response to this query.
    Malformed,
    /// The server says the name does not exist.
    NoSuchName,
    /// The server reports a failure; the value is its response code.
    ServerFailure(u8),
    /// The name exists but has no IPv4 address.
    NoAddress,
    /// The answer did not fit and was cut short.
    Truncated,
}

/// Writes a query for the IPv4 address of `name`, with identifier `id`,
/// at the start of `buffer` and returns its length. One trailing dot is
/// accepted. `buffer` must hold a header, the name and four more bytes
/// (273 bytes are always enough).
pub fn write_query(buffer: &mut [u8], id: u16, name: &str) -> Result<usize, NameError> {
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > 253 {
        return Err(NameError::TooLong);
    }
    // Header, each label with its length byte, the root label, type, class.
    let total = HEADER_LEN + name.len() + 2 + 4;
    if buffer.len() < total {
        return Err(NameError::TooLong);
    }
    buffer[..HEADER_LEN].fill(0);
    buffer[0..2].copy_from_slice(&id.to_be_bytes());
    buffer[2..4].copy_from_slice(&FLAG_RECURSION_DESIRED.to_be_bytes());
    buffer[4..6].copy_from_slice(&1u16.to_be_bytes());
    let mut at = HEADER_LEN;
    for label in name.split('.') {
        if label.is_empty() {
            return Err(NameError::Empty);
        }
        if label.len() > 63 {
            return Err(NameError::TooLong);
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(NameError::BadCharacter);
        }
        buffer[at] = label.len() as u8;
        buffer[at + 1..at + 1 + label.len()].copy_from_slice(label.as_bytes());
        at += 1 + label.len();
    }
    buffer[at] = 0;
    buffer[at + 1..at + 3].copy_from_slice(&TYPE_A.to_be_bytes());
    buffer[at + 3..at + 5].copy_from_slice(&CLASS_IN.to_be_bytes());
    Ok(at + 5)
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

/// The offset just after the name that starts at `at`. Names are
/// sequences of length-prefixed labels ending with a zero length or with
/// a two-byte pointer to an earlier name, which is not followed.
fn skip_name(data: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let length = *data.get(at)?;
        match length & 0xC0 {
            0xC0 => return (at + 2 <= data.len()).then_some(at + 2),
            0 if length == 0 => return Some(at + 1),
            0 => at += 1 + usize::from(length),
            _ => return None,
        }
    }
}

/// Reads the response `data` to the query with identifier `id`: the
/// first IPv4 address among its answers. With a recursive server that is
/// the address of the name asked for, whether directly or through
/// aliases; the owner names of the records are not compared.
pub fn parse_response(data: &[u8], id: u16) -> Result<[u8; 4], ResponseError> {
    use ResponseError::Malformed;
    let flags = be16(data, 2).ok_or(Malformed)?;
    if data.len() < HEADER_LEN
        || be16(data, 0) != Some(id)
        || flags & FLAG_RESPONSE == 0
        || flags & OPCODE_MASK != 0
    {
        return Err(Malformed);
    }
    match flags & RCODE_MASK {
        0 => {}
        RCODE_NAME_ERROR => return Err(ResponseError::NoSuchName),
        code => return Err(ResponseError::ServerFailure(code as u8)),
    }
    let questions = be16(data, 4).ok_or(Malformed)?;
    let answers = be16(data, 6).ok_or(Malformed)?;
    let mut at = HEADER_LEN;
    for _ in 0..questions {
        at = skip_name(data, at).ok_or(Malformed)? + 4;
    }
    for _ in 0..answers {
        // A truncated message may end anywhere in its answers.
        let truncated = flags & FLAG_TRUNCATED != 0;
        let cut = if truncated {
            ResponseError::Truncated
        } else {
            Malformed
        };
        at = skip_name(data, at).ok_or(cut)?;
        let kind = be16(data, at).ok_or(cut)?;
        let class = be16(data, at + 2).ok_or(cut)?;
        let length = usize::from(be16(data, at + 8).ok_or(cut)?);
        let value = data.get(at + 10..at + 10 + length).ok_or(cut)?;
        if kind == TYPE_A && class == CLASS_IN {
            let address: [u8; 4] = value.try_into().map_err(|_| Malformed)?;
            return Ok(address);
        }
        at += 10 + length;
    }
    if flags & FLAG_TRUNCATED != 0 {
        return Err(ResponseError::Truncated);
    }
    Err(ResponseError::NoAddress)
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::vec::Vec;

    #[test]
    fn a_query_has_the_bytes_the_rfc_describes() {
        let mut buffer = [0xEEu8; 64];
        let length = write_query(&mut buffer, 0xABCD, "www.example.org").unwrap();
        let expected: &[u8] = &[
            0xAB, 0xCD, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, // header
            3, b'w', b'w', b'w', 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'o', b'r', b'g',
            0, // name
            0, 1, 0, 1, // A, IN
        ];
        assert_eq!(&buffer[..length], expected);
        assert_eq!(buffer[length], 0xEE);
        // A trailing dot means the same.
        let mut dotted = [0u8; 64];
        let dotted_length = write_query(&mut dotted, 0xABCD, "www.example.org.").unwrap();
        assert_eq!(&dotted[..dotted_length], expected);
    }

    #[test]
    fn bad_names_are_refused() {
        let mut buffer = [0u8; 512];
        let long_label = "a".repeat(64);
        let long_name = ["abcdefgh"; 29].join(".");
        assert_eq!(long_name.len(), 260);
        for (name, error) in [
            ("", NameError::Empty),
            (".", NameError::Empty),
            ("a..b", NameError::Empty),
            (".a", NameError::Empty),
            ("a.b..", NameError::Empty),
            ("exa mple.org", NameError::BadCharacter),
            ("exämple.org", NameError::BadCharacter),
            ("a/b", NameError::BadCharacter),
            (long_label.as_str(), NameError::TooLong),
            (long_name.as_str(), NameError::TooLong),
        ] {
            assert_eq!(write_query(&mut buffer, 1, name), Err(error), "{name:?}");
        }
        assert!(write_query(&mut buffer, 1, &"a".repeat(63)).is_ok());
        assert!(write_query(&mut buffer, 1, "my_host-1.lan").is_ok());
        // A buffer that is one byte short.
        assert_eq!(write_query(&mut [0; 22], 1, "a.bc"), Ok(22));
        assert_eq!(
            write_query(&mut [0; 21], 1, "a.bc"),
            Err(NameError::TooLong)
        );
    }

    /// A response to a query for `name` with the given flags and answer
    /// records (name as a pointer to the question, then type, class,
    /// TTL, data).
    fn response(id: u16, flags: u16, records: &[(u16, &[u8])]) -> Vec<u8> {
        let mut query = [0u8; 64];
        let length = write_query(&mut query, id, "host.example").unwrap();
        let mut message = query[..length].to_vec();
        message[2..4].copy_from_slice(&flags.to_be_bytes());
        message[6..8].copy_from_slice(&(records.len() as u16).to_be_bytes());
        for (kind, value) in records {
            message.extend_from_slice(&[0xC0, 12]);
            message.extend_from_slice(&kind.to_be_bytes());
            message.extend_from_slice(&[0, 1, 0, 0, 0, 60]);
            message.extend_from_slice(&(value.len() as u16).to_be_bytes());
            message.extend_from_slice(value);
        }
        message
    }

    const OK: u16 = 0x8180;

    #[test]
    fn an_address_is_found() {
        let message = response(7, OK, &[(1, &[93, 184, 216, 34])]);
        assert_eq!(parse_response(&message, 7), Ok([93, 184, 216, 34]));
    }

    #[test]
    fn an_alias_before_the_address_is_skipped() {
        // CNAME (type 5) whose data is a name, then the address.
        let alias: &[u8] = &[5, b'o', b't', b'h', b'e', b'r', 0xC0, 17];
        let message = response(
            7,
            OK,
            &[(5, alias), (1, &[10, 1, 2, 3]), (1, &[10, 9, 9, 9])],
        );
        assert_eq!(parse_response(&message, 7), Ok([10, 1, 2, 3]));
    }

    #[test]
    fn answers_without_an_address() {
        assert_eq!(
            parse_response(&response(7, OK, &[]), 7),
            Err(ResponseError::NoAddress)
        );
        // Only an IPv6 address (type 28).
        let message = response(7, OK, &[(28, &[0; 16])]);
        assert_eq!(parse_response(&message, 7), Err(ResponseError::NoAddress));
        assert_eq!(
            parse_response(&response(7, 0x8183, &[]), 7),
            Err(ResponseError::NoSuchName)
        );
        assert_eq!(
            parse_response(&response(7, 0x8182, &[]), 7),
            Err(ResponseError::ServerFailure(2))
        );
    }

    #[test]
    fn responses_to_other_queries_and_non_responses_are_refused() {
        let message = response(7, OK, &[(1, &[1, 2, 3, 4])]);
        assert_eq!(parse_response(&message, 8), Err(ResponseError::Malformed));
        // The query itself (no response flag).
        let query = response(7, 0x0100, &[(1, &[1, 2, 3, 4])]);
        assert_eq!(parse_response(&query, 7), Err(ResponseError::Malformed));
        // An address record of the wrong size.
        let odd = response(7, OK, &[(1, &[1, 2, 3])]);
        assert_eq!(parse_response(&odd, 7), Err(ResponseError::Malformed));
    }

    #[test]
    fn truncation_is_reported_as_such() {
        // Flag set and nothing usable.
        assert_eq!(
            parse_response(&response(7, OK | 0x0200, &[]), 7),
            Err(ResponseError::Truncated)
        );
        // Flag set and the message cut inside a record.
        let message = response(7, OK | 0x0200, &[(5, &[1, b'x', 0]), (1, &[1, 2, 3, 4])]);
        assert_eq!(
            parse_response(&message[..message.len() - 6], 7),
            Err(ResponseError::Truncated)
        );
        // A complete address in a truncated message is still an address.
        let message = response(7, OK | 0x0200, &[(1, &[1, 2, 3, 4])]);
        assert_eq!(parse_response(&message, 7), Ok([1, 2, 3, 4]));
    }

    #[test]
    fn no_input_makes_it_panic_or_loop() {
        let good = response(7, OK, &[(5, &[1, b'x', 0]), (1, &[1, 2, 3, 4])]);
        for length in 0..good.len() {
            assert!(
                parse_response(&good[..length], 7).is_err(),
                "cut at {length}"
            );
        }
        // Every single-byte change gives some answer or some error.
        for at in 0..good.len() {
            for value in [0u8, 0x3F, 0x40, 0x80, 0xC0, 0xFF] {
                let mut bad = good.clone();
                bad[at] = value;
                let _ = parse_response(&bad, 7);
            }
        }
        // Counts far larger than the message.
        let mut greedy = good.clone();
        greedy[4..8].fill(0xFF);
        assert_eq!(parse_response(&greedy, 7), Err(ResponseError::Malformed));
    }
}
