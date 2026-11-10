// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Terminal line editing.
//!
//! Turns the raw byte stream from a terminal into complete lines, echoing
//! what is typed. Deliberately minimal: printable ASCII, backspace, Ctrl-C
//! (abandon the line) and Ctrl-U (erase the line). Escape sequences such as
//! arrow keys are recognised and ignored so they do not end up in the line.

use alloc::string::String;
use core::fmt::Write;

/// Longest accepted line; further input is ignored (with a bell).
pub const MAX_LINE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// Saw ESC.
    Started,
    /// Inside a CSI sequence (`ESC [` ... final byte).
    Csi,
}

/// Accumulates input bytes into lines.
#[derive(Debug)]
pub struct LineEditor {
    line: String,
    escape: Escape,
    /// The previous byte was a carriage return, so a following line feed
    /// belongs to the same line ending.
    after_cr: bool,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl LineEditor {
    /// Creates an editor with an empty line.
    pub const fn new() -> Self {
        Self {
            line: String::new(),
            escape: Escape::None,
            after_cr: false,
        }
    }

    /// Whether nothing has been typed on the current line.
    pub fn is_empty(&self) -> bool {
        self.line.is_empty()
    }

    /// Processes one input byte, writing any echo to `echo`. Returns the
    /// finished line when the byte completes one.
    pub fn feed(&mut self, byte: u8, echo: &mut dyn Write) -> Option<String> {
        let after_cr = core::mem::take(&mut self.after_cr);

        match self.escape {
            Escape::Started => {
                self.escape = if byte == b'[' {
                    Escape::Csi
                } else {
                    Escape::None
                };
                return None;
            }
            Escape::Csi => {
                // Parameter and intermediate bytes are 0x20-0x3F; anything
                // from 0x40 to 0x7E ends the sequence.
                if (0x40..=0x7E).contains(&byte) {
                    self.escape = Escape::None;
                }
                return None;
            }
            Escape::None => {}
        }

        match byte {
            b'\r' => {
                self.after_cr = true;
                let _ = echo.write_str("\n");
                Some(core::mem::take(&mut self.line))
            }
            b'\n' if after_cr => None,
            b'\n' => {
                let _ = echo.write_str("\n");
                Some(core::mem::take(&mut self.line))
            }
            0x08 | 0x7F => {
                if self.line.pop().is_some() {
                    let _ = echo.write_str("\x08 \x08");
                }
                None
            }
            0x03 => {
                self.line.clear();
                let _ = echo.write_str("^C\n");
                Some(String::new())
            }
            0x15 => {
                for _ in 0..self.line.len() {
                    let _ = echo.write_str("\x08 \x08");
                }
                self.line.clear();
                None
            }
            0x1B => {
                self.escape = Escape::Started;
                None
            }
            0x20..=0x7E => {
                if self.line.len() < MAX_LINE {
                    self.line.push(char::from(byte));
                    let _ = echo.write_char(char::from(byte));
                } else {
                    let _ = echo.write_str("\x07");
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Feeds `input` and returns the completed lines and everything echoed.
    fn run(input: &[u8]) -> (Vec<String>, String) {
        let mut editor = LineEditor::new();
        let mut echo = String::new();
        let lines = input
            .iter()
            .filter_map(|&b| editor.feed(b, &mut echo))
            .collect();
        (lines, echo)
    }

    #[test]
    fn plain_lines_with_any_line_ending() {
        assert_eq!(run(b"ls -l\r").0, ["ls -l"]);
        assert_eq!(run(b"a\nb\n").0, ["a", "b"]);
        assert_eq!(run(b"a\r\nb\r\n").0, ["a", "b"], "CR LF is one ending");
        assert_eq!(run(b"\r\r").0, ["", ""]);
        assert_eq!(
            run(b"a\r\n\n").0,
            ["a", ""],
            "only the first LF is swallowed"
        );
        let (_, echo) = run(b"hi\r");
        assert_eq!(echo, "hi\n");
    }

    #[test]
    fn backspace_and_kill_line() {
        let (lines, echo) = run(b"lx\x7fs\r");
        assert_eq!(lines, ["ls"]);
        assert_eq!(echo, "lx\x08 \x08s\n");
        assert_eq!(run(b"\x7f\x08ok\r").0, ["ok"], "backspace on an empty line");
        assert_eq!(run(b"junk\x15ls\r").0, ["ls"]);
    }

    #[test]
    fn ctrl_c_abandons_the_line() {
        let (lines, echo) = run(b"rm -r /\x03ls\r");
        assert_eq!(lines, ["", "ls"]);
        assert!(echo.contains("^C\n"));
    }

    #[test]
    fn escape_sequences_and_control_bytes_are_dropped() {
        assert_eq!(run(b"a\x1b[Ab\x1b[1;5Cc\x1bOd\r").0, ["abcd"]);
        assert_eq!(run(b"a\x00\x01\x80\xffb\r").0, ["ab"]);
    }

    #[test]
    fn line_length_is_bounded() {
        let mut input = alloc::vec![b'x'; MAX_LINE + 10];
        input.push(b'\r');
        let (lines, echo) = run(&input);
        assert_eq!(lines[0].len(), MAX_LINE);
        assert_eq!(echo.matches('\x07').count(), 10);
    }
}
