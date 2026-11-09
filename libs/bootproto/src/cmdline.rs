// SPDX-License-Identifier: GPL-2.0-only
//! Kernel command line.
//!
//! The command line is a single line of ASCII text made of
//! whitespace-separated tokens. A token is either a bare flag (`quiet`) or a
//! `key=value` pair (`fault=panic`). When a key appears more than once the
//! last occurrence wins. There is no quoting.

/// A parsed view over a kernel command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cmdline<'a> {
    text: &'a str,
}

impl<'a> Cmdline<'a> {
    /// An empty command line.
    pub const EMPTY: Self = Self { text: "" };

    /// Wraps `text`. Never fails: unknown tokens are simply never matched.
    pub fn new(text: &'a str) -> Self {
        Self { text: text.trim() }
    }

    /// Builds a command line from raw bytes handed over by the loader.
    /// Returns `None` unless the bytes are printable ASCII or whitespace.
    pub fn from_bytes(bytes: &'a [u8]) -> Option<Self> {
        if !bytes
            .iter()
            .all(|b| b.is_ascii_graphic() || b.is_ascii_whitespace())
        {
            return None;
        }
        core::str::from_utf8(bytes).ok().map(Self::new)
    }

    /// The trimmed command line text.
    pub fn as_str(&self) -> &'a str {
        self.text
    }

    /// Iterates over `(key, value)` tokens; bare flags have no value.
    pub fn tokens(&self) -> impl Iterator<Item = (&'a str, Option<&'a str>)> {
        self.text
            .split_ascii_whitespace()
            .map(|tok| match tok.split_once('=') {
                Some((key, value)) => (key, Some(value)),
                None => (tok, None),
            })
    }

    /// Whether the bare flag `name` is present.
    pub fn flag(&self, name: &str) -> bool {
        self.tokens().any(|(k, v)| k == name && v.is_none())
    }

    /// The value of the last `name=value` token, if any.
    pub fn value(&self, name: &str) -> Option<&'a str> {
        self.tokens()
            .filter(|(k, _)| *k == name)
            .filter_map(|(_, v)| v)
            .last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_values() {
        let c = Cmdline::new("  qemu-exit fault=panic\tlog=debug  \n");
        assert_eq!(c.as_str(), "qemu-exit fault=panic\tlog=debug");
        assert!(c.flag("qemu-exit"));
        assert!(!c.flag("fault"));
        assert!(!c.flag("qemu"));
        assert_eq!(c.value("fault"), Some("panic"));
        assert_eq!(c.value("log"), Some("debug"));
        assert_eq!(c.value("qemu-exit"), None);
        assert_eq!(c.value("missing"), None);
    }

    #[test]
    fn last_value_wins_and_empty_values_are_kept() {
        let c = Cmdline::new("a=1 a=2 b= c=x=y");
        assert_eq!(c.value("a"), Some("2"));
        assert_eq!(c.value("b"), Some(""));
        assert_eq!(c.value("c"), Some("x=y"));
    }

    #[test]
    fn empty_command_line() {
        assert_eq!(Cmdline::new("   "), Cmdline::EMPTY);
        assert_eq!(Cmdline::EMPTY.tokens().count(), 0);
        assert_eq!(Cmdline::from_bytes(b""), Some(Cmdline::EMPTY));
    }

    #[test]
    fn rejects_non_printable_bytes() {
        assert!(Cmdline::from_bytes(b"ok=1\n").is_some());
        assert!(Cmdline::from_bytes(b"bad\0").is_none());
        assert!(Cmdline::from_bytes(b"bad\x1b[2J").is_none());
        assert!(Cmdline::from_bytes("naïve".as_bytes()).is_none());
    }
}
