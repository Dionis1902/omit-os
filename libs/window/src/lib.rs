// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The window protocol: how a program with a window talks to the desktop.
//!
//! A program connects to the channel [`CHANNEL`] and sends
//! [`Request::Create`]. The desktop answers [`Event::Created`] with the
//! token of a shared memory segment that holds the window's pixels, row
//! by row, `0x00RRGGBB`. The program maps the segment, draws into it and
//! sends [`Request::Present`] for the area it changed; the desktop sends
//! key presses and pointer events. One connection is one window; closing
//! the connection — from either side — closes the window.
//!
//! Messages are small frames on a byte stream: a two-byte length (of
//! what follows), a two-byte kind, and the fields in little-endian
//! order. This crate only encodes and decodes; it makes no system calls
//! and is tested on the build host, including against arbitrary bytes:
//! each side must survive whatever the other sends.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// The name of the channel the desktop listens on.
pub const CHANNEL: &str = "desktop";

/// Longest window title, in bytes.
pub const TITLE_MAX: usize = 64;
/// Largest window content area, in pixels per side.
pub const SIZE_MAX: u32 = 4096;
/// Largest payload of a frame.
const PAYLOAD_MAX: usize = 16 + TITLE_MAX;

/// What a program asks of the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Open the connection's window with a content area of `width` x
    /// `height` pixels. Sent once, first.
    Create {
        /// Width of the content area.
        width: u32,
        /// Height of the content area.
        height: u32,
        /// Shown in the title bar.
        title: String,
    },
    /// The pixels in this rectangle of the content area have changed and
    /// should be shown.
    Present {
        /// Left edge.
        x: u32,
        /// Top edge.
        y: u32,
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
    /// Change the text in the title bar.
    SetTitle {
        /// The new title.
        title: String,
    },
}

/// What the desktop tells a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The window exists. Its pixels are the first `width * height * 4`
    /// bytes of the shared memory segment `token`.
    Created {
        /// Token of the shared memory segment.
        token: u64,
        /// Width of the content area, as requested.
        width: u32,
        /// Height of the content area, as requested.
        height: u32,
    },
    /// A key was pressed while the window had the keyboard. Codes are
    /// those of the `input_read` system call.
    Key {
        /// The key.
        code: i32,
    },
    /// The pointer moved or a button changed, from a press of the left
    /// button in the content area until its release. Positions are
    /// relative to the content area and can lie outside it.
    Pointer {
        /// Pixels from the left edge.
        x: i32,
        /// Pixels from the top edge.
        y: i32,
        /// Buttons held: bit 0 is the left button.
        buttons: u32,
    },
}

/// A frame that does not follow the protocol. The connection cannot be
/// used any further: the position in the stream is lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

mod kind {
    pub const CREATE: u16 = 1;
    pub const PRESENT: u16 = 2;
    pub const SET_TITLE: u16 = 3;
    pub const CREATED: u16 = 0x101;
    pub const KEY: u16 = 0x102;
    pub const POINTER: u16 = 0x103;
}

fn frame(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + payload.len());
    bytes.extend_from_slice(&((payload.len() + 2) as u16).to_le_bytes());
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// The longest prefix of `title` that fits in [`TITLE_MAX`] bytes.
fn clip_title(title: &str) -> &str {
    let mut end = title.len().min(TITLE_MAX);
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    &title[..end]
}

fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn word(payload: &[u8], index: usize) -> Result<u32, Malformed> {
    payload
        .get(index * 4..index * 4 + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(Malformed)
}

fn title(bytes: &[u8]) -> Result<String, Malformed> {
    if bytes.len() > TITLE_MAX {
        return Err(Malformed);
    }
    let text = core::str::from_utf8(bytes).map_err(|_| Malformed)?;
    if text.chars().any(char::is_control) {
        return Err(Malformed);
    }
    Ok(text.into())
}

impl Request {
    /// The bytes to send for this request. Titles longer than
    /// [`TITLE_MAX`] are cut.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Create {
                width,
                height,
                title,
            } => {
                let mut payload = words(&[*width, *height]);
                payload.extend_from_slice(clip_title(title).as_bytes());
                frame(kind::CREATE, &payload)
            }
            Self::Present {
                x,
                y,
                width,
                height,
            } => frame(kind::PRESENT, &words(&[*x, *y, *width, *height])),
            Self::SetTitle { title } => frame(kind::SET_TITLE, clip_title(title).as_bytes()),
        }
    }

    fn decode(kind: u16, payload: &[u8]) -> Result<Self, Malformed> {
        match kind {
            kind::CREATE => {
                let (width, height) = (word(payload, 0)?, word(payload, 1)?);
                if width == 0 || height == 0 || width > SIZE_MAX || height > SIZE_MAX {
                    return Err(Malformed);
                }
                Ok(Self::Create {
                    width,
                    height,
                    title: title(&payload[8..])?,
                })
            }
            kind::PRESENT if payload.len() == 16 => Ok(Self::Present {
                x: word(payload, 0)?,
                y: word(payload, 1)?,
                width: word(payload, 2)?,
                height: word(payload, 3)?,
            }),
            kind::SET_TITLE => Ok(Self::SetTitle {
                title: title(payload)?,
            }),
            _ => Err(Malformed),
        }
    }
}

impl Event {
    /// The bytes to send for this event.
    pub fn encode(&self) -> Vec<u8> {
        match *self {
            Self::Created {
                token,
                width,
                height,
            } => frame(
                kind::CREATED,
                &words(&[token as u32, (token >> 32) as u32, width, height]),
            ),
            Self::Key { code } => frame(kind::KEY, &words(&[code as u32])),
            Self::Pointer { x, y, buttons } => {
                frame(kind::POINTER, &words(&[x as u32, y as u32, buttons]))
            }
        }
    }

    fn decode(kind: u16, payload: &[u8]) -> Result<Self, Malformed> {
        match (kind, payload.len()) {
            (kind::CREATED, 16) => Ok(Self::Created {
                token: u64::from(word(payload, 0)?) | (u64::from(word(payload, 1)?) << 32),
                width: word(payload, 2)?,
                height: word(payload, 3)?,
            }),
            (kind::KEY, 4) => Ok(Self::Key {
                code: word(payload, 0)? as i32,
            }),
            (kind::POINTER, 12) => Ok(Self::Pointer {
                x: word(payload, 0)? as i32,
                y: word(payload, 1)? as i32,
                buttons: word(payload, 2)?,
            }),
            _ => Err(Malformed),
        }
    }
}

/// Turns the bytes arriving on a connection back into messages.
#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    failed: bool,
}

impl Decoder {
    /// A decoder with nothing received yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds bytes read from the connection.
    pub fn push(&mut self, bytes: &[u8]) {
        if !self.failed {
            self.buffer.extend_from_slice(bytes);
        }
    }

    /// The next complete frame: its kind and payload. `Ok(None)` if more
    /// bytes are needed.
    fn next_frame(&mut self) -> Result<Option<(u16, Vec<u8>)>, Malformed> {
        if self.failed {
            return Err(Malformed);
        }
        let Some(header) = self.buffer.get(..4) else {
            return Ok(None);
        };
        let length = usize::from(u16::from_le_bytes([header[0], header[1]]));
        let kind = u16::from_le_bytes([header[2], header[3]]);
        if !(2..=PAYLOAD_MAX + 2).contains(&length) {
            self.failed = true;
            self.buffer.clear();
            return Err(Malformed);
        }
        let end = 2 + length;
        if self.buffer.len() < end {
            return Ok(None);
        }
        let payload = self.buffer[4..end].to_vec();
        self.buffer.drain(..end);
        Ok(Some((kind, payload)))
    }

    fn next_with<T>(
        &mut self,
        decode: impl FnOnce(u16, &[u8]) -> Result<T, Malformed>,
    ) -> Result<Option<T>, Malformed> {
        let Some((kind, payload)) = self.next_frame()? else {
            return Ok(None);
        };
        match decode(kind, &payload) {
            Ok(message) => Ok(Some(message)),
            Err(error) => {
                self.failed = true;
                self.buffer.clear();
                Err(error)
            }
        }
    }

    /// The next request, for the desktop's side of a connection.
    /// `Ok(None)` means more bytes are needed; after an error the decoder
    /// only returns errors.
    pub fn next_request(&mut self) -> Result<Option<Request>, Malformed> {
        self.next_with(Request::decode)
    }

    /// The next event, for the program's side of a connection.
    pub fn next_event(&mut self) -> Result<Option<Event>, Malformed> {
        self.next_with(Event::decode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn requests() -> Vec<Request> {
        vec![
            Request::Create {
                width: 320,
                height: 200,
                title: "Clock".into(),
            },
            Request::Create {
                width: SIZE_MAX,
                height: 1,
                title: String::new(),
            },
            Request::Present {
                x: 1,
                y: 2,
                width: 3,
                height: u32::MAX,
            },
            Request::SetTitle {
                title: "Über".into(),
            },
        ]
    }

    fn events() -> Vec<Event> {
        vec![
            Event::Created {
                token: 0xFEDC_BA98_7654_3210,
                width: 640,
                height: 480,
            },
            Event::Key { code: 0x101 },
            Event::Key { code: -1 },
            Event::Pointer {
                x: -5,
                y: 7,
                buttons: 1,
            },
        ]
    }

    #[test]
    fn round_trips_in_one_piece_and_byte_by_byte() {
        let stream: Vec<u8> = requests().iter().flat_map(Request::encode).collect();
        let mut decoder = Decoder::new();
        decoder.push(&stream);
        for expected in requests() {
            assert_eq!(decoder.next_request(), Ok(Some(expected)));
        }
        assert_eq!(decoder.next_request(), Ok(None));

        let stream: Vec<u8> = events().iter().flat_map(Event::encode).collect();
        let mut decoder = Decoder::new();
        let mut decoded = Vec::new();
        for byte in stream {
            decoder.push(&[byte]);
            while let Some(event) = decoder.next_event().unwrap() {
                decoded.push(event);
            }
        }
        assert_eq!(decoded, events());
    }

    #[test]
    fn long_titles_are_cut_at_a_character() {
        let long = "é".repeat(TITLE_MAX);
        let mut decoder = Decoder::new();
        decoder.push(&Request::SetTitle { title: long }.encode());
        let Ok(Some(Request::SetTitle { title })) = decoder.next_request() else {
            panic!("not decoded");
        };
        assert_eq!(title, "é".repeat(TITLE_MAX / 2));
    }

    #[test]
    fn rejects_what_the_protocol_does_not_allow() {
        let bad: Vec<Vec<u8>> = vec![
            // Length below the kind field's size, and above the maximum.
            vec![1, 0, 1, 0],
            vec![0xFF, 0xFF, 1, 0],
            // Unknown kind.
            frame(99, &[]),
            // An event is not a request.
            Event::Key { code: 1 }.encode(),
            // Sizes of zero and beyond the maximum.
            frame(kind::CREATE, &words(&[0, 10])),
            frame(kind::CREATE, &words(&[10, SIZE_MAX + 1])),
            // Truncated and oversized payloads.
            frame(kind::CREATE, &[1, 0, 0]),
            frame(kind::PRESENT, &words(&[1, 2, 3])),
            frame(kind::PRESENT, &words(&[1, 2, 3, 4, 5])),
            // Titles: invalid UTF-8, control characters.
            frame(kind::SET_TITLE, &[0xFF]),
            frame(kind::SET_TITLE, b"a\nb"),
        ];
        for bytes in bad {
            let mut decoder = Decoder::new();
            decoder.push(&bytes);
            assert_eq!(decoder.next_request(), Err(Malformed), "{bytes:?}");
            // And it stays failed, whatever follows.
            decoder.push(&Request::SetTitle { title: "x".into() }.encode());
            assert_eq!(decoder.next_request(), Err(Malformed));
        }
        let mut decoder = Decoder::new();
        decoder.push(&requests()[0].encode());
        assert_eq!(decoder.next_event(), Err(Malformed));
    }

    #[test]
    fn arbitrary_bytes_never_panic_or_grow_without_bound() {
        // A fixed pseudo-random stream (xorshift), fed in odd pieces.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let mut decoder = Decoder::new();
            for _ in 0..8 {
                let piece: Vec<u8> = (0..next() % 40).map(|_| next() as u8).collect();
                decoder.push(&piece);
                for _ in 0..4 {
                    let _ = decoder.next_request();
                    let _ = decoder.next_event();
                }
                assert!(decoder.buffer.len() <= 4 + PAYLOAD_MAX + 40);
            }
        }
    }
}
