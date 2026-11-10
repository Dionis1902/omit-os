// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The interface's glyph icons, as ready-drawn coverage masks.
//!
//! The icons are line drawings on a 24-unit grid from the Omit OS design
//! system (`libs/gfx/icons/icons.json`). `tools/gen-icons.js` draws each
//! at the sizes the interface uses; the result, `glyphs.omi`, is built
//! into this library. Like a font's glyphs, a mask is blended in
//! whatever colour is asked for.
//!
//! File format (integers little-endian): `"OMI1"`, mask count (u32); one
//! 24-byte record per mask — name (16 bytes, zero-padded), size in
//! pixels (u8), three zero bytes, offset (u32) of its `size * size`
//! coverage bytes in what follows the records.

static GLYPHS: &[u8] = include_bytes!("../icons/glyphs.omi");

const HEADER_LEN: usize = 8;
const RECORD_LEN: usize = 24;

/// A square picture to be drawn in one colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mask {
    /// Width and height in pixels.
    pub size: i32,
    /// `size * size` coverage values, row by row.
    pub coverage: &'static [u8],
}

fn find(data: &'static [u8], name: &str, size: i32) -> Option<Mask> {
    if !data.starts_with(b"OMI1") || name.is_empty() || name.len() > 16 {
        return None;
    }
    let count = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?) as usize;
    let masks = HEADER_LEN.checked_add(count.checked_mul(RECORD_LEN)?)?;
    for index in 0..count {
        let record =
            data.get(HEADER_LEN + index * RECORD_LEN..HEADER_LEN + (index + 1) * RECORD_LEN)?;
        let stored = &record[..16];
        let matches = stored.starts_with(name.as_bytes())
            && stored[name.len()..].iter().all(|&byte| byte == 0);
        if matches && i32::from(record[16]) == size {
            let offset =
                u32::from_le_bytes([record[20], record[21], record[22], record[23]]) as usize;
            let start = masks.checked_add(offset)?;
            let length = (size * size) as usize;
            return Some(Mask {
                size,
                coverage: data.get(start..start.checked_add(length)?)?,
            });
        }
    }
    None
}

/// The icon called `name` at `size` pixels (13, 16, 20 or 24), if there
/// is one.
pub fn icon(name: &str, size: i32) -> Option<Mask> {
    find(GLYPHS, name, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;

    #[test]
    fn the_icons_the_interface_uses_exist_at_every_size() {
        for name in [
            "start", "search", "folder", "doc", "term", "gear", "info", "clock", "play", "brush",
            "power", "close", "minimize", "maximize", "restore", "check", "chevron", "trash",
            "home", "drive", "img", "calc", "mon", "undo", "battery",
        ] {
            for size in [13, 16, 20, 24] {
                let mask = icon(name, size).unwrap_or_else(|| panic!("{name} at {size}"));
                assert_eq!(mask.size, size);
                assert_eq!(mask.coverage.len(), (size * size) as usize);
                // Something is drawn, and not everything is.
                // (Thin strokes that fall between two rows of pixels are
                // drawn as two rows of half coverage.)
                let solid = mask.coverage.iter().filter(|&&c| c > 64).count();
                assert!(
                    solid > 4 && solid < (size * size) as usize * 3 / 4,
                    "{name} at {size}"
                );
            }
        }
    }

    #[test]
    fn a_line_drawing_looks_like_its_subject() {
        // "minimize" is one horizontal stroke through the middle.
        let mask = icon("minimize", 16).unwrap();
        let row = |y: usize| {
            mask.coverage[y * 16..(y + 1) * 16]
                .iter()
                .filter(|&&c| c > 64)
                .count()
        };
        assert!(row(8) + row(7) >= 8);
        assert_eq!(row(2), 0);
        assert_eq!(row(13), 0);
        // "close" is a cross: its centre is drawn, the middle of its edges is not.
        let mask = icon("close", 16).unwrap();
        let at = |x: usize, y: usize| mask.coverage[y * 16 + x];
        assert!(u32::from(at(7, 7)) + u32::from(at(8, 8)) > 200);
        assert_eq!(at(8, 1), 0);
        assert_eq!(at(1, 8), 0);
    }

    #[test]
    fn names_and_sizes_that_do_not_exist() {
        assert_eq!(icon("no-such-icon", 16), None);
        assert_eq!(icon("close", 17), None);
        assert_eq!(icon("", 16), None);
        // A name that is the beginning of another is not that other.
        assert!(icon("cal", 16).is_some() && icon("calc", 16).is_some());
        assert_ne!(icon("cal", 16), icon("calc", 16));
        assert_eq!(icon("clos", 16), None);
        assert_eq!(icon("a-name-longer-than-sixteen", 16), None);
    }

    #[test]
    fn damaged_icon_files_have_no_icons_and_do_not_panic() {
        for length in [0, 3, 4, 7, 8, 30, 100, GLYPHS.len() - 1] {
            for name in ["close", "wx", "atlas"] {
                let _ = find(&GLYPHS[..length], name, 24);
            }
        }
        let mut huge = [0u8; 32];
        huge[..4].copy_from_slice(b"OMI1");
        huge[4..8].fill(0xFF);
        assert_eq!(find(Box::leak(Box::new(huge)), "close", 16), None);
    }
}
