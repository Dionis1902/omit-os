// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The pictures of the Omit OS design system: the wallpapers and the
//! programs' icons.
//!
//! The originals are vector drawings (`libs/gfx/art/*.svg`).
//! `tools/gen-art.js` draws them on the build machine into plain pixel
//! files, which are built into this library: each icon at the sizes the
//! interface uses, with its transparency; each wallpaper small, to be
//! scaled up to the screen by [`Picture::scaled`].
//!
//! File format: `"OMP1"`, width and height (u16, little-endian), bytes
//! per pixel (u8: 3 for red, green, blue; 4 with opacity), three zero
//! bytes, then the pixels row by row.

use alloc::vec::Vec;

use crate::Colour;
use crate::paint::mix;
use crate::theme::Mode;

const HEADER_LEN: usize = 12;

/// A picture: a rectangle of pixels, with or without transparency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picture {
    width: i32,
    height: i32,
    channels: usize,
    pixels: &'static [u8],
}

impl Picture {
    /// Reads a picture file; `None` if it is not one or is cut short.
    pub fn parse(data: &'static [u8]) -> Option<Self> {
        if !data.starts_with(b"OMP1") || data.len() < HEADER_LEN {
            return None;
        }
        let width = usize::from(u16::from_le_bytes([data[4], data[5]]));
        let height = usize::from(u16::from_le_bytes([data[6], data[7]]));
        let channels = usize::from(data[8]);
        if width == 0 || height == 0 || !(channels == 3 || channels == 4) {
            return None;
        }
        let pixels = data.get(HEADER_LEN..HEADER_LEN + width * height * channels)?;
        Some(Self {
            width: width as i32,
            height: height as i32,
            channels,
            pixels,
        })
    }

    /// Width in pixels.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// The colour and opacity (0–255) of the pixel at (`x`, `y`), which
    /// must be inside the picture.
    pub fn pixel(&self, x: i32, y: i32) -> (Colour, u8) {
        let at = (y * self.width + x) as usize * self.channels;
        let colour = (Colour::from(self.pixels[at]) << 16)
            | (Colour::from(self.pixels[at + 1]) << 8)
            | Colour::from(self.pixels[at + 2]);
        let alpha = if self.channels == 4 {
            self.pixels[at + 3]
        } else {
            255
        };
        (colour, alpha)
    }

    /// The colour at (`x`, `y`) of this picture enlarged (or reduced) to
    /// cover a `width` x `height` area: scaled by the same factor in
    /// both directions, centred, what does not fit cut off; in-between
    /// pixels blended from their four neighbours. For opaque pictures.
    pub fn scaled(&self, width: i32, height: i32, x: i32, y: i32) -> Colour {
        let step = self.step(width, height);
        let across = Self::place(x, width, self.width, step);
        let down = Self::place(y, height, self.height, step);
        self.blend(across, down)
    }

    /// The whole of what [`Picture::scaled`] gives for a `width` x
    /// `height` area, row by row: the same colours, worked out with the
    /// places along each axis found once instead of for every pixel.
    pub fn scaled_to(&self, width: i32, height: i32) -> Vec<Colour> {
        let (width, height) = (width.max(0), height.max(0));
        let step = self.step(width, height);
        let across: Vec<Place> = (0..width)
            .map(|x| Self::place(x, width, self.width, step))
            .collect();
        let mut pixels = Vec::with_capacity(width as usize * height as usize);
        for y in 0..height {
            let down = Self::place(y, height, self.height, step);
            pixels.extend(across.iter().map(|&across| self.blend(across, down)));
        }
        pixels
    }

    /// Source pixels per target pixel, in 1/65536: the smaller of the
    /// two ratios, so that the picture covers the area.
    fn step(&self, width: i32, height: i32) -> i64 {
        let (w, h) = (i64::from(width.max(1)), i64::from(height.max(1)));
        let (pw, ph) = (i64::from(self.width), i64::from(self.height));
        ((pw << 16) / w).min((ph << 16) / h).max(1)
    }

    /// Where the centre of target pixel `position` (of `target` along
    /// this axis) falls among the `source` pixels of the picture: the
    /// pixel before it, the one after, and how far between the two.
    fn place(position: i32, target: i32, source: i32, step: i64) -> Place {
        let (target, source) = (i64::from(target.max(1)), i64::from(source));
        let offset = ((source << 16) - target * step) / 2;
        let at = (offset + i64::from(position) * step + step / 2 - (1 << 15))
            .clamp(0, (source - 1) << 16);
        let before = (at >> 16) as i32;
        Place {
            before,
            after: (before + 1).min(source as i32 - 1),
            between: ((at >> 8) & 0xFF) as u8,
        }
    }

    fn blend(&self, across: Place, down: Place) -> Colour {
        let top = mix(
            self.pixel(across.after, down.before).0,
            self.pixel(across.before, down.before).0,
            across.between,
        );
        let bottom = mix(
            self.pixel(across.after, down.after).0,
            self.pixel(across.before, down.after).0,
            across.between,
        );
        mix(bottom, top, down.between)
    }
}

/// A place between two neighbouring pixels along one axis.
#[derive(Clone, Copy)]
struct Place {
    before: i32,
    after: i32,
    /// How far from `before` towards `after`, in 1/256.
    between: u8,
}

/// The wallpaper for `mode`: "Dawn" in light mode, "Dusk" in dark.
pub fn wallpaper(mode: Mode) -> Option<Picture> {
    Picture::parse(match mode {
        Mode::Light => include_bytes!("../art/wallpaper-dawn.omp"),
        Mode::Dark => include_bytes!("../art/wallpaper-dusk.omp"),
    })
}

/// The icon of the program (or thing) called `name`, `size` pixels wide
/// and high (20 or 30), if the design system has drawn one.
pub fn icon(name: &str, size: i32) -> Option<Picture> {
    macro_rules! pictures {
        ($($name:literal => $file:literal),* $(,)?) => {
            match (name, size) {
                $(
                    ($name, 20) => Some(&include_bytes!(concat!("../art/", $file, "-20.omp"))[..]),
                    ($name, 30) => Some(&include_bytes!(concat!("../art/", $file, "-30.omp"))[..]),
                )*
                _ => None,
            }
        };
    }
    let data: Option<&'static [u8]> = pictures! {
        "files" => "app-files",
        "terminal" => "app-terminal",
        "settings" => "app-settings",
        "clock" => "app-clock",
        "canvas" => "app-canvas",
        "player" => "app-player",
        "notes" => "app-notes",
        "calculator" => "app-calculator",
        "folder" => "folder",
    };
    Picture::parse(data?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_picture_is_there_and_well_formed() {
        for name in [
            "files",
            "terminal",
            "settings",
            "clock",
            "canvas",
            "player",
            "notes",
            "folder",
            "calculator",
            "calculator",
        ] {
            for size in [20, 30] {
                let picture = icon(name, size).unwrap_or_else(|| panic!("{name} at {size}"));
                assert_eq!((picture.width(), picture.height()), (size, size));
                // An icon has solid parts and leaves its corners clear.
                let solid = (0..size)
                    .flat_map(|y| (0..size).map(move |x| (x, y)))
                    .filter(|&(x, y)| picture.pixel(x, y).1 > 200)
                    .count();
                assert!(
                    solid > (size * size / 5) as usize,
                    "{name} at {size}: {solid}"
                );
                assert!(picture.pixel(0, 0).1 < 40, "{name} at {size}");
            }
            assert_eq!(icon(name, 24), None);
        }
        assert_eq!(icon("no such program", 20), None);
        for mode in [Mode::Light, Mode::Dark] {
            let picture = wallpaper(mode).unwrap();
            assert_eq!((picture.width(), picture.height()), (320, 180));
            assert_eq!(picture.pixel(10, 10).1, 255, "wallpapers are opaque");
        }
        // Dawn is light, dusk is dark.
        let brightness = |mode| {
            let picture = wallpaper(mode).unwrap();
            let (colour, _) = picture.pixel(160, 60);
            (colour >> 16) + ((colour >> 8) & 0xFF) + (colour & 0xFF)
        };
        assert!(brightness(Mode::Light) > 2 * brightness(Mode::Dark));
    }

    /// A 2 x 2 picture: black, red / green, white.
    fn checks() -> Picture {
        static DATA: [u8; 24] = [
            b'O', b'M', b'P', b'1', 2, 0, 2, 0, 3, 0, 0, 0, // header
            0, 0, 0, 255, 0, 0, // black, red
            0, 255, 0, 255, 255, 255, // green, white
        ];
        Picture::parse(&DATA).unwrap()
    }

    /// The whole-area scaling is a faster way to the same colours.
    #[test]
    fn scaling_a_whole_area_gives_what_scaling_each_pixel_gives() {
        for mode in [Mode::Light, Mode::Dark] {
            let picture = wallpaper(mode).unwrap();
            for (width, height) in [(1280, 800), (640, 480), (97, 301), (1, 1), (0, 5)] {
                let whole = picture.scaled_to(width, height);
                assert_eq!(whole.len(), (width * height) as usize);
                for y in 0..height {
                    for x in 0..width {
                        assert_eq!(
                            whole[(y * width + x) as usize],
                            picture.scaled(width, height, x, y),
                            "{width}x{height} at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn scaling_keeps_corners_and_blends_between() {
        let picture = checks();
        // At its own size: itself.
        assert_eq!(picture.scaled(2, 2, 0, 0), 0x00_00_00);
        assert_eq!(picture.scaled(2, 2, 1, 0), 0xFF_00_00);
        assert_eq!(picture.scaled(2, 2, 0, 1), 0x00_FF_00);
        assert_eq!(picture.scaled(2, 2, 1, 1), 0xFF_FF_FF);
        // Enlarged: the corners keep their colours, and along the top
        // edge black turns into red steadily.
        assert_eq!(picture.scaled(100, 100, 0, 0), 0x00_00_00);
        assert_eq!(picture.scaled(100, 100, 99, 0), 0xFF_00_00);
        assert_eq!(picture.scaled(100, 100, 0, 99), 0x00_FF_00);
        assert_eq!(picture.scaled(100, 100, 99, 99), 0xFF_FF_FF);
        let reds: std::vec::Vec<u32> = (0..100)
            .map(|x| picture.scaled(100, 100, x, 0) >> 16)
            .collect();
        assert!(reds.windows(2).all(|pair| pair[0] <= pair[1]), "{reds:?}");
        assert!((100..160).contains(&reds[50]), "{}", reds[50]);
        // The middle is the average of all four: half red, half green,
        // a quarter blue.
        let middle = picture.scaled(100, 100, 50, 50);
        assert!((115..140).contains(&(middle >> 16)), "{middle:06x}");
        assert!((115..140).contains(&((middle >> 8) & 0xFF)), "{middle:06x}");
        assert!((55..75).contains(&(middle & 0xFF)), "{middle:06x}");
    }

    #[test]
    fn scaling_covers_areas_of_another_shape_by_cutting_off() {
        let picture = checks();
        // A wide area shows the picture's full width and the middle of
        // its height: left is between black and green, right between
        // red and white.
        let left = picture.scaled(200, 50, 0, 25);
        let right = picture.scaled(200, 50, 199, 25);
        assert_eq!(left >> 16, 0);
        assert_eq!(right >> 16, 0xFF);
        // A tall area: the full height, the middle of the width.
        let top = picture.scaled(50, 200, 25, 0);
        let bottom = picture.scaled(50, 200, 25, 199);
        assert_eq!((top >> 8) & 0xFF, 0);
        assert_eq!((bottom >> 8) & 0xFF, 0xFF);
        // Degenerate sizes and far-off positions do not panic.
        let _ = picture.scaled(0, 0, 0, 0);
        let _ = picture.scaled(1, 1, 0, 0);
        let _ = picture.scaled(5, 5, -100, 10_000);
        let _ = picture.scaled(100_000, 3, 99_999, 2);
    }

    #[test]
    fn damaged_picture_files_are_refused() {
        static GOOD: &[u8] = include_bytes!("../art/folder-20.omp");
        assert!(Picture::parse(GOOD).is_some());
        for length in [0, 3, 4, 11, 12, 100, GOOD.len() - 1] {
            assert_eq!(Picture::parse(&GOOD[..length]), None, "{length} bytes");
        }
        static BAD_CHANNELS: [u8; 20] = [
            b'O', b'M', b'P', b'1', 1, 0, 1, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(Picture::parse(&BAD_CHANNELS), None);
        static NO_SIZE: [u8; 12] = [b'O', b'M', b'P', b'1', 0, 0, 9, 0, 3, 0, 0, 0];
        assert_eq!(Picture::parse(&NO_SIZE), None);
        assert_eq!(Picture::parse(b"PNG-not-a-picture-of-ours"), None);
    }
}
