// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Text conversion.
//!
//! Windows has two versions of every function that takes text: `...W`
//! takes UTF-16, `...A` takes text in the "ANSI code page". This layer
//! reports the code page as UTF-8 (65001), which Windows supports, so
//! `...A` text is UTF-8 and nothing needs converting on that side.

use alloc::string::String;
use alloc::vec::Vec;

/// The UTF-8 code page identifier.
pub const CP_UTF8: u32 = 65001;

/// Converts UTF-16 to UTF-8. Unpaired surrogates, which are not valid
/// text, become U+FFFD, as in Windows' own conversions.
pub fn from_utf16(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Converts UTF-8 to NUL-terminated UTF-16.
pub fn to_utf16z(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_both_ways() {
        let wide = to_utf16z("héllo ☃ 😀");
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(from_utf16(&wide[..wide.len() - 1]), "héllo ☃ 😀");
        // 😀 needs a surrogate pair.
        assert_eq!(to_utf16z("😀"), [0xD83D, 0xDE00, 0]);
        assert_eq!(from_utf16(&[0x41, 0xD800, 0x42]), "A\u{FFFD}B");
        assert_eq!(from_utf16(&[]), "");
    }
}
