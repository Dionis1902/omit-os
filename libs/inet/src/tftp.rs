// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The packets of TFTP (RFC 1350), as far as reading a file needs them:
//! the request, data blocks, acknowledgements and errors.
//!
//! A transfer is: the client sends a read request to port 69; the server
//! answers from a port of its choosing with block 1; the client
//! acknowledges each block to that port and the server sends the next;
//! a block shorter than 512 bytes is the last. No options (RFC 2347),
//! so no block sizes other than 512 and no transfer sizes; no writing.

/// UDP port servers listen on for requests.
pub const PORT: u16 = 69;
/// Bytes of file data in every block but the last.
pub const BLOCK_LEN: usize = 512;
/// Longest packet: opcode, block number, one block.
pub const MAX_PACKET_LEN: usize = 4 + BLOCK_LEN;

const OP_READ_REQUEST: u16 = 1;
const OP_DATA: u16 = 3;
const OP_ACK: u16 = 4;
const OP_ERROR: u16 = 5;

/// Writes a request to read `file` as raw bytes ("octet" mode) at the
/// start of `buffer`; returns the packet's length, or `None` if the name
/// is empty, contains a zero byte or does not fit.
pub fn write_read_request(buffer: &mut [u8], file: &str) -> Option<usize> {
    const MODE: &[u8] = b"octet";
    let name = file.as_bytes();
    let total = 2 + name.len() + 1 + MODE.len() + 1;
    if name.is_empty() || name.contains(&0) || buffer.len() < total {
        return None;
    }
    buffer[..2].copy_from_slice(&OP_READ_REQUEST.to_be_bytes());
    buffer[2..2 + name.len()].copy_from_slice(name);
    buffer[2 + name.len()] = 0;
    let mode_at = 3 + name.len();
    buffer[mode_at..mode_at + MODE.len()].copy_from_slice(MODE);
    buffer[total - 1] = 0;
    Some(total)
}

/// Writes the acknowledgement of block `block`; returns its length (4),
/// or `None` if the buffer is too small.
pub fn write_ack(buffer: &mut [u8], block: u16) -> Option<usize> {
    let out = buffer.get_mut(..4)?;
    out[..2].copy_from_slice(&OP_ACK.to_be_bytes());
    out[2..].copy_from_slice(&block.to_be_bytes());
    Some(4)
}

/// What a server sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    /// A block of the file.
    Data {
        /// Its number, starting at 1.
        block: u16,
        /// Up to 512 bytes; fewer means this is the last block.
        data: &'a [u8],
    },
    /// The transfer has failed.
    Error {
        /// The server's error code (1: file not found, 2: access
        /// violation, ...).
        code: u16,
        /// The server's explanation; not necessarily text.
        message: &'a [u8],
    },
}

impl<'a> Packet<'a> {
    /// Parses a packet from a server; `None` if it is neither data nor
    /// an error, or malformed.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        let number = u16::from_be_bytes([data[2], data[3]]);
        let rest = &data[4..];
        match u16::from_be_bytes([data[0], data[1]]) {
            OP_DATA if rest.len() <= BLOCK_LEN => Some(Self::Data {
                block: number,
                data: rest,
            }),
            OP_ERROR => {
                let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
                Some(Self::Error {
                    code: number,
                    message: &rest[..end],
                })
            }
            _ => None,
        }
    }
}

/// What the client should do with a data block, given the number of the
/// block it is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The block is the one expected: keep its data and acknowledge it.
    /// `last` is set if the transfer is complete.
    Accept {
        /// Whether this was the final block.
        last: bool,
    },
    /// A block already received (its acknowledgement was lost, or the
    /// server repeated itself): acknowledge it again, keep nothing.
    Repeat,
    /// Anything else: ignore it.
    Ignore,
}

/// Decides what to do with block `block` of `length` bytes while
/// waiting for block `expected`. Block numbers wrap around after 65535,
/// which is how files larger than 32 MiB are transferred in practice.
pub fn step(expected: u16, block: u16, length: usize) -> Step {
    if block == expected {
        Step::Accept {
            last: length < BLOCK_LEN,
        }
    } else if block == expected.wrapping_sub(1) {
        Step::Repeat
    } else {
        Step::Ignore
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_matches_the_rfc() {
        let mut buffer = [0xEEu8; 32];
        let length = write_read_request(&mut buffer, "boot/hello.txt").unwrap();
        assert_eq!(&buffer[..length], b"\x00\x01boot/hello.txt\x00octet\x00");
        assert_eq!(buffer[length], 0xEE);
    }

    #[test]
    fn requests_that_cannot_be_made_are_refused() {
        let mut buffer = [0u8; 32];
        assert_eq!(write_read_request(&mut buffer, ""), None);
        assert_eq!(write_read_request(&mut buffer, "a\0b"), None);
        assert_eq!(write_read_request(&mut [0; 9], "a"), None);
        assert_eq!(write_read_request(&mut [0; 10], "a"), Some(10));
    }

    #[test]
    fn acknowledgements() {
        let mut buffer = [0u8; 4];
        assert_eq!(write_ack(&mut buffer, 0x0102), Some(4));
        assert_eq!(buffer, [0, 4, 1, 2]);
        assert_eq!(write_ack(&mut [0; 3], 1), None);
    }

    #[test]
    fn data_and_errors_are_recognised() {
        assert_eq!(
            Packet::parse(&[0, 3, 0, 7, b'a', b'b']),
            Some(Packet::Data {
                block: 7,
                data: b"ab"
            })
        );
        // The empty last block of a file whose size is a multiple of 512.
        assert_eq!(
            Packet::parse(&[0, 3, 1, 0]),
            Some(Packet::Data {
                block: 256,
                data: b""
            })
        );
        assert_eq!(
            Packet::parse(b"\x00\x05\x00\x01File not found\x00"),
            Some(Packet::Error {
                code: 1,
                message: b"File not found"
            })
        );
        // An error message without its terminator is taken as it is.
        assert_eq!(
            Packet::parse(b"\x00\x05\x00\x02no"),
            Some(Packet::Error {
                code: 2,
                message: b"no"
            })
        );
    }

    #[test]
    fn other_packets_are_refused() {
        assert_eq!(Packet::parse(&[]), None);
        assert_eq!(Packet::parse(&[0, 3, 0]), None);
        // An acknowledgement, a request, an unknown opcode.
        assert_eq!(Packet::parse(&[0, 4, 0, 1]), None);
        assert_eq!(Packet::parse(b"\x00\x01f\x00octet\x00"), None);
        assert_eq!(Packet::parse(&[0, 9, 0, 0]), None);
        // More than a block of data.
        let mut long = [0u8; 4 + 513];
        long[1] = 3;
        assert_eq!(Packet::parse(&long), None);
        assert!(Packet::parse(&long[..4 + 512]).is_some());
    }

    #[test]
    fn blocks_are_accepted_in_order_only() {
        assert_eq!(step(1, 1, 512), Step::Accept { last: false });
        assert_eq!(step(1, 1, 511), Step::Accept { last: true });
        assert_eq!(step(1, 1, 0), Step::Accept { last: true });
        assert_eq!(step(5, 4, 512), Step::Repeat);
        assert_eq!(step(5, 6, 512), Step::Ignore);
        assert_eq!(step(5, 3, 512), Step::Ignore);
        // Around the wrap.
        assert_eq!(step(0, 0, 512), Step::Accept { last: false });
        assert_eq!(step(0, 65535, 512), Step::Repeat);
        assert_eq!(step(65535, 65534, 10), Step::Repeat);
    }
}
