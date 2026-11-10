// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Proportional text: the Inter typeface, as small files of ready-drawn
//! glyphs.
//!
//! The system has no font rasteriser. `tools/gen-font.py` draws the
//! characters the interface needs, at the sizes it uses, on the build
//! machine; the results are in `libs/gfx/fonts/` and are built into
//! this library. A glyph is a rectangle of coverage values (0 = nothing,
//! 255 = solid) to blend in whatever colour is asked for.
//!
//! File format (all integers little-endian): `"OMF1"`, size in pixels
//! (u8), ascent (u8), descent (u8), one zero byte, glyph count (u16), two
//! zero bytes; then one 16-byte record per glyph, sorted by code point —
//! code point (u32), advance (u8), left (i8) and top (i8) of the bitmap
//! relative to the pen and the top of the line, width (u8), height (u8),
//! three zero bytes, offset of the bitmap (u32) in what follows the
//! records.
//!
//! Inter is © The Inter Project Authors, SIL Open Font License 1.1; see
//! `ATTRIBUTION.md`.

const HEADER_LEN: usize = 12;
const RECORD_LEN: usize = 16;

/// One typeface at one size.
#[derive(Debug, Clone, Copy)]
pub struct Font {
    data: &'static [u8],
}

/// A character's picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// How far the pen moves.
    pub advance: i32,
    /// Where the bitmap's left edge is, relative to the pen.
    pub left: i32,
    /// Where the bitmap's top edge is, relative to the top of the line.
    pub top: i32,
    /// Width of the bitmap.
    pub width: i32,
    /// Height of the bitmap.
    pub height: i32,
    /// `width * height` coverage values, row by row.
    pub coverage: &'static [u8],
}

/// Regular, 11 px: the smallest labels.
pub const REGULAR_11: Font = Font::new(include_bytes!("../fonts/inter-regular-11.omf"));
/// Regular, 12 px: secondary text.
pub const REGULAR_12: Font = Font::new(include_bytes!("../fonts/inter-regular-12.omf"));
/// Regular, 13 px: body text.
pub const REGULAR_13: Font = Font::new(include_bytes!("../fonts/inter-regular-13.omf"));
/// Medium, 13 px: window titles, selected items.
pub const MEDIUM_13: Font = Font::new(include_bytes!("../fonts/inter-medium-13.omf"));
/// Semibold, 13 px: emphasis, the clock.
pub const SEMIBOLD_13: Font = Font::new(include_bytes!("../fonts/inter-semibold-13.omf"));
/// Semibold, 15 px: section headings.
pub const SEMIBOLD_15: Font = Font::new(include_bytes!("../fonts/inter-semibold-15.omf"));
/// Semibold, 20 px: page titles.
pub const SEMIBOLD_20: Font = Font::new(include_bytes!("../fonts/inter-semibold-20.omf"));
/// Semibold, 32 px: large figures.
pub const SEMIBOLD_32: Font = Font::new(include_bytes!("../fonts/inter-semibold-32.omf"));

impl Font {
    /// A font over the bytes of a font file. A file that is damaged is
    /// not rejected here; it simply has no glyphs where it is damaged.
    pub const fn new(data: &'static [u8]) -> Self {
        Self { data }
    }

    fn header(&self, at: usize) -> i32 {
        if self.data.len() >= HEADER_LEN && self.data.starts_with(b"OMF1") {
            i32::from(self.data[at])
        } else {
            0
        }
    }

    /// Distance from the top of a line to the baseline.
    pub fn ascent(&self) -> i32 {
        self.header(5)
    }

    /// Height of a line of text: ascent plus descent.
    pub fn height(&self) -> i32 {
        self.header(5) + self.header(6)
    }

    fn count(&self) -> usize {
        if self.header(4) == 0 {
            return 0;
        }
        usize::from(u16::from_le_bytes([self.data[8], self.data[9]]))
    }

    fn record(&self, index: usize) -> Option<&'static [u8]> {
        let at = HEADER_LEN + index * RECORD_LEN;
        self.data.get(at..at + RECORD_LEN)
    }

    /// The picture of `character`, if the font has it.
    pub fn glyph(&self, character: char) -> Option<Glyph> {
        let wanted = u32::from(character);
        let (mut low, mut high) = (0, self.count());
        while low < high {
            let middle = (low + high) / 2;
            let record = self.record(middle)?;
            let code = u32::from_le_bytes([record[0], record[1], record[2], record[3]]);
            if code == wanted {
                let (width, height) = (usize::from(record[7]), usize::from(record[8]));
                let offset =
                    u32::from_le_bytes([record[12], record[13], record[14], record[15]]) as usize;
                let start = HEADER_LEN + self.count() * RECORD_LEN + offset;
                return Some(Glyph {
                    advance: i32::from(record[4]),
                    left: i32::from(record[5] as i8),
                    top: i32::from(record[6] as i8),
                    width: width as i32,
                    height: height as i32,
                    coverage: self.data.get(start..start + width * height)?,
                });
            }
            if code < wanted {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        None
    }

    /// The glyph drawn for `character`: its own, or `?` for characters
    /// the font lacks.
    pub fn glyph_or_fallback(&self, character: char) -> Option<Glyph> {
        self.glyph(character).or_else(|| self.glyph('?'))
    }

    /// Width of `text` in pixels.
    pub fn width(&self, text: &str) -> i32 {
        text.chars()
            .filter_map(|character| self.glyph_or_fallback(character))
            .map(|glyph| glyph.advance)
            .sum()
    }

    /// The longest beginning of `text` that is at most `width` pixels
    /// wide; if that is not all of it, it ends in `…` (and may be empty
    /// if not even that fits).
    pub fn fit<'a>(&self, text: &'a str, width: i32) -> (&'a str, bool) {
        if self.width(text) <= width {
            return (text, false);
        }
        let room = width - self.width("…");
        let mut used = 0;
        let mut end = 0;
        for (at, character) in text.char_indices() {
            let advance = self
                .glyph_or_fallback(character)
                .map_or(0, |glyph| glyph.advance);
            if used + advance > room {
                break;
            }
            used += advance;
            end = at + character.len_utf8();
        }
        (&text[..end], true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(&str, Font, i32); 8] = [
        ("regular 11", REGULAR_11, 11),
        ("regular 12", REGULAR_12, 12),
        ("regular 13", REGULAR_13, 13),
        ("medium 13", MEDIUM_13, 13),
        ("semibold 13", SEMIBOLD_13, 13),
        ("semibold 15", SEMIBOLD_15, 15),
        ("semibold 20", SEMIBOLD_20, 20),
        ("semibold 32", SEMIBOLD_32, 32),
    ];

    #[test]
    fn every_font_has_every_printable_ascii_character() {
        for (name, font, size) in ALL {
            assert!(
                font.ascent() >= size * 8 / 10 && font.ascent() <= size * 12 / 10,
                "{name}"
            );
            assert!(
                font.height() > font.ascent() && font.height() <= size * 15 / 10,
                "{name}"
            );
            for code in 32u8..127 {
                let character = char::from(code);
                let glyph = font
                    .glyph(character)
                    .unwrap_or_else(|| panic!("{name}: {character:?}"));
                assert!(glyph.advance > 0, "{name}: {character:?}");
                assert_eq!(glyph.coverage.len(), (glyph.width * glyph.height) as usize);
                // A glyph stays within its line, give or take a pixel of overshoot.
                assert!(
                    glyph.top >= -2 && glyph.top + glyph.height <= font.height() + 2,
                    "{name}: {character:?}"
                );
                // Everything but the space draws something.
                assert_eq!(
                    glyph.coverage.iter().any(|&c| c > 64),
                    character != ' ',
                    "{name}: {character:?}"
                );
            }
            for character in "…–—·×›•’“”→✓".chars() {
                assert!(font.glyph(character).is_some(), "{name}: {character:?}");
            }
        }
    }

    #[test]
    fn letters_have_the_shapes_one_expects() {
        let font = REGULAR_13;
        // Proportional: narrow letters are narrower than wide ones.
        assert!(font.width("i") < font.width("m"));
        assert!(font.width("l") * 2 < font.width("W"));
        assert_eq!(font.width("ab"), font.width("a") + font.width("b"));
        assert_eq!(font.width(""), 0);
        // Capitals stand on the baseline; a "g" goes below it.
        let top_and_bottom = |c| {
            let glyph = font.glyph(c).unwrap();
            (glyph.top, glyph.top + glyph.height)
        };
        assert_eq!(top_and_bottom('H').1, font.ascent());
        assert!(top_and_bottom('g').1 > font.ascent());
        assert!(top_and_bottom('x').0 > top_and_bottom('H').0);
        // Heavier weights are wider; larger sizes are larger.
        assert!(SEMIBOLD_13.width("Settings") > REGULAR_13.width("Settings"));
        assert!(SEMIBOLD_20.width("Settings") > SEMIBOLD_15.width("Settings"));
    }

    #[test]
    fn characters_the_font_lacks_become_question_marks() {
        let font = REGULAR_13;
        assert_eq!(font.glyph('\u{4E2D}'), None);
        assert_eq!(font.glyph_or_fallback('\u{4E2D}'), font.glyph('?'));
        assert_eq!(font.width("\u{4E2D}\u{6587}"), 2 * font.width("?"));
    }

    #[test]
    fn text_is_cut_to_fit_with_an_ellipsis() {
        let font = REGULAR_13;
        let text = "Brand Guidelines v3.pdf";
        assert_eq!(font.fit(text, 1000), (text, false));
        assert_eq!(font.fit(text, font.width(text)), (text, false));
        let (cut, shortened) = font.fit(text, 80);
        assert!(shortened);
        assert!(text.starts_with(cut) && cut.len() < text.len() && !cut.is_empty());
        assert!(font.width(cut) + font.width("…") <= 80);
        // One more character would not have fitted.
        let next = &text[..cut.len() + 1];
        assert!(font.width(next) + font.width("…") > 80);
        assert_eq!(font.fit(text, 3), ("", true));
        // Cutting never splits a character.
        let (cut, _) = font.fit("ääääääää", 30);
        assert!(cut.chars().all(|c| c == 'ä'));
    }

    #[test]
    fn damaged_font_files_have_no_glyphs_and_do_not_panic() {
        let good: &'static [u8] = include_bytes!("../fonts/inter-regular-13.omf");
        for length in [0, 3, 4, 11, 12, 20, 500, good.len() - 1] {
            let font = Font::new(&good[..length]);
            let _ = (font.ascent(), font.height(), font.width("Hello, world"));
            for code in 32u8..127 {
                let _ = font.glyph(char::from(code));
            }
        }
        assert_eq!(Font::new(b"NOPE-not-a-font-file").width("abc"), 0);
        assert_eq!(Font::new(b"NOPE-not-a-font-file").height(), 0);
    }
}
