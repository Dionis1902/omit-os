// SPDX-License-Identifier: GPL-2.0-only
//! Images in memory and drawing on them.

use alloc::vec;
use alloc::vec::Vec;

use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};

use crate::Colour;
use crate::geometry::{Point, Rect};

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const FONT_HEIGHT: RasterHeight = RasterHeight::Size16;

/// Height of a line of text, in pixels.
pub const TEXT_HEIGHT: i32 = FONT_HEIGHT.val() as i32;
/// Width of one character, in pixels (the font is monospaced).
pub const CHAR_WIDTH: i32 = get_raster_width(FONT_WEIGHT, FONT_HEIGHT) as i32;

/// The width of `text` when drawn, in pixels.
pub fn text_width(text: &str) -> i32 {
    (text.chars().count() as i32).saturating_mul(CHAR_WIDTH)
}

/// Mixes `fg` over `bg` with coverage `alpha` (0 = all background).
fn blend(fg: Colour, bg: Colour, alpha: u8) -> Colour {
    let alpha = u32::from(alpha);
    let mix = |shift: u32| {
        let f = (fg >> shift) & 0xFF;
        let b = (bg >> shift) & 0xFF;
        ((f * alpha + b * (255 - alpha)) / 255) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// A rectangular image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    width: usize,
    height: usize,
    pixels: Vec<Colour>,
}

impl Surface {
    /// Creates a `width` x `height` image filled with `colour`.
    pub fn new(width: usize, height: usize, colour: Colour) -> Self {
        Self {
            width,
            height,
            pixels: vec![colour; width * height],
        }
    }

    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The whole image as a rectangle.
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// The pixels, row by row, `width` per row.
    pub fn pixels(&self) -> &[Colour] {
        &self.pixels
    }

    /// The colour at (`x`, `y`), if that is inside the image.
    pub fn pixel(&self, x: i32, y: i32) -> Option<Colour> {
        self.bounds()
            .contains(Point::new(x, y))
            .then(|| self.pixels[y as usize * self.width + x as usize])
    }

    /// A canvas for drawing anywhere on the image.
    pub fn canvas(&mut self) -> Canvas<'_> {
        let clip = self.bounds();
        Canvas {
            surface: self,
            clip,
            origin: Point::default(),
        }
    }
}

/// A view of a [`Surface`] to draw through. Coordinates are relative to
/// the canvas's origin, and nothing is drawn outside its clip rectangle,
/// so code that is given a canvas for part of an image cannot touch the
/// rest.
#[derive(Debug)]
pub struct Canvas<'a> {
    surface: &'a mut Surface,
    /// In surface coordinates.
    clip: Rect,
    /// Where (0, 0) of this canvas is on the surface.
    origin: Point,
}

impl Canvas<'_> {
    /// A canvas for the part `area` (in this canvas's coordinates) of this
    /// one: its origin is `area`'s corner, and drawing is clipped to both
    /// `area` and this canvas's clip.
    pub fn sub(&mut self, area: Rect) -> Canvas<'_> {
        let absolute = area.translate(self.origin.x, self.origin.y);
        Canvas {
            clip: self.clip.intersect(&absolute),
            origin: Point::new(absolute.x, absolute.y),
            surface: self.surface,
        }
    }

    /// This canvas with drawing further restricted to `area` (in this
    /// canvas's coordinates); the origin stays where it is.
    pub fn clipped(&mut self, area: Rect) -> Canvas<'_> {
        let absolute = area.translate(self.origin.x, self.origin.y);
        Canvas {
            clip: self.clip.intersect(&absolute),
            origin: self.origin,
            surface: self.surface,
        }
    }

    /// The part of the canvas that drawing can reach, in its own
    /// coordinates. Useful for skipping work that would be clipped anyway.
    pub fn visible(&self) -> Rect {
        self.clip.translate(-self.origin.x, -self.origin.y)
    }

    /// Fills `area` with `colour`.
    pub fn fill(&mut self, area: Rect, colour: Colour) {
        let target = area
            .translate(self.origin.x, self.origin.y)
            .intersect(&self.clip);
        if target.is_empty() {
            return;
        }
        // The clip lies inside the surface, so these are in range.
        let width = self.surface.width;
        let (left, right) = (target.x as usize, target.right() as usize);
        for y in target.y as usize..target.bottom() as usize {
            self.surface.pixels[y * width + left..y * width + right].fill(colour);
        }
    }

    /// Draws a one-pixel outline just inside `area`.
    pub fn outline(&mut self, area: Rect, colour: Colour) {
        if area.is_empty() {
            return;
        }
        self.fill(Rect::new(area.x, area.y, area.w, 1), colour);
        self.fill(Rect::new(area.x, area.bottom() - 1, area.w, 1), colour);
        self.fill(Rect::new(area.x, area.y, 1, area.h), colour);
        self.fill(Rect::new(area.right() - 1, area.y, 1, area.h), colour);
    }

    /// Sets one pixel.
    pub fn set(&mut self, x: i32, y: i32, colour: Colour) {
        self.fill(Rect::new(x, y, 1, 1), colour);
    }

    /// Draws `text` in `colour` with its top-left corner at (`x`, `y`),
    /// over whatever is already there. Characters the font lacks are
    /// drawn as `?`. Returns the x coordinate after the last character.
    pub fn text(&mut self, x: i32, y: i32, text: &str, colour: Colour) -> i32 {
        let mut pen = x;
        for ch in text.chars() {
            let cell =
                Rect::new(pen, y, CHAR_WIDTH, TEXT_HEIGHT).translate(self.origin.x, self.origin.y);
            let target = cell.intersect(&self.clip);
            if !target.is_empty() {
                let glyph = get_raster(ch, FONT_WEIGHT, FONT_HEIGHT)
                    .or_else(|| get_raster('?', FONT_WEIGHT, FONT_HEIGHT));
                if let Some(glyph) = glyph {
                    let width = self.surface.width;
                    for py in target.y..target.bottom() {
                        for px in target.x..target.right() {
                            let coverage = glyph
                                .pixel((px - cell.x) as usize, (py - cell.y) as usize)
                                .unwrap_or(0);
                            if coverage != 0 {
                                let pixel =
                                    &mut self.surface.pixels[py as usize * width + px as usize];
                                *pixel = blend(colour, *pixel, coverage);
                            }
                        }
                    }
                }
            }
            pen = pen.saturating_add(CHAR_WIDTH);
        }
        pen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Colour = 0xFF0000;
    const WHITE: Colour = 0xFFFFFF;

    fn count(surface: &Surface, colour: Colour) -> usize {
        surface.pixels().iter().filter(|&&p| p == colour).count()
    }

    #[test]
    fn fill_is_clipped_to_the_surface() {
        let mut surface = Surface::new(10, 8, 0);
        surface.canvas().fill(Rect::new(-5, -5, 8, 8), RED);
        assert_eq!(count(&surface, RED), 9);
        assert_eq!(surface.pixel(2, 2), Some(RED));
        assert_eq!(surface.pixel(3, 2), Some(0));
        surface.canvas().fill(Rect::new(8, 6, 100, 100), WHITE);
        assert_eq!(count(&surface, WHITE), 4);
        surface
            .canvas()
            .fill(Rect::new(i32::MAX, i32::MIN, i32::MAX, 3), WHITE);
        surface.canvas().fill(Rect::new(50, 50, 5, 5), WHITE);
        assert_eq!(count(&surface, WHITE), 4);
        assert_eq!(surface.pixel(10, 0), None);
        assert_eq!(surface.pixel(-1, 0), None);
    }

    #[test]
    fn sub_canvas_translates_and_confines() {
        let mut surface = Surface::new(20, 20, 0);
        let mut canvas = surface.canvas();
        let mut window = canvas.sub(Rect::new(5, 5, 10, 10));
        assert_eq!(window.visible(), Rect::new(0, 0, 10, 10));
        // Far larger than the window: only the window's area changes.
        window.fill(Rect::new(-100, -100, 1000, 1000), RED);
        let mut inner = window.sub(Rect::new(8, 8, 10, 10));
        inner.fill(Rect::new(0, 0, 10, 10), WHITE);
        assert_eq!(count(&surface, RED), 96);
        assert_eq!(count(&surface, WHITE), 4);
        assert_eq!(surface.pixel(4, 5), Some(0));
        assert_eq!(surface.pixel(5, 5), Some(RED));
        assert_eq!(surface.pixel(14, 14), Some(WHITE));
        assert_eq!(surface.pixel(15, 15), Some(0));
    }

    #[test]
    fn clipped_keeps_the_origin() {
        let mut surface = Surface::new(20, 20, 0);
        let mut canvas = surface.canvas();
        let mut part = canvas.clipped(Rect::new(10, 0, 5, 20));
        part.fill(Rect::new(0, 0, 20, 1), RED);
        assert_eq!(count(&surface, RED), 5);
        assert_eq!(surface.pixel(10, 0), Some(RED));
        assert_eq!(surface.pixel(9, 0), Some(0));
    }

    #[test]
    fn outline_draws_only_the_border() {
        let mut surface = Surface::new(10, 10, 0);
        surface.canvas().outline(Rect::new(2, 2, 6, 5), RED);
        assert_eq!(count(&surface, RED), 2 * 6 + 2 * 3);
        assert_eq!(surface.pixel(2, 2), Some(RED));
        assert_eq!(surface.pixel(7, 6), Some(RED));
        assert_eq!(surface.pixel(3, 3), Some(0));
        surface.canvas().outline(Rect::default(), WHITE);
        assert_eq!(count(&surface, WHITE), 0);
    }

    #[test]
    fn text_draws_glyphs_inside_their_cells() {
        let (w, h) = (CHAR_WIDTH as usize * 4, TEXT_HEIGHT as usize);
        let mut surface = Surface::new(w, h, 0);
        let end = surface.canvas().text(0, 0, "Hi", WHITE);
        assert_eq!(end, 2 * CHAR_WIDTH);
        assert_eq!(text_width("Hi"), 2 * CHAR_WIDTH);
        let lit = |surface: &Surface, cell: i32| {
            (0..TEXT_HEIGHT)
                .flat_map(|y| (0..CHAR_WIDTH).map(move |x| (x, y)))
                .filter(|&(x, y)| surface.pixel(cell * CHAR_WIDTH + x, y) != Some(0))
                .count()
        };
        assert!(lit(&surface, 0) > 10, "H is drawn");
        assert!(lit(&surface, 1) > 5, "i is drawn");
        assert_eq!(lit(&surface, 2), 0, "nothing after the text");
        // The strokes are bright; anti-aliasing never exceeds the text
        // colour, and it is grey (all channels equal) for white on black.
        assert!(surface.pixels().iter().any(|&p| p & 0xFF > 0xB0));
        assert!(surface.pixels().iter().all(|&p| p >> 16 == p & 0xFF));

        // A space draws nothing; an unknown character draws something.
        let mut other = Surface::new(w, h, 0);
        other.canvas().text(0, 0, " \u{1F600}", WHITE);
        assert_eq!(lit(&other, 0), 0);
        assert!(lit(&other, 1) > 0);
    }

    #[test]
    fn text_is_clipped() {
        let mut surface = Surface::new(40, 40, 0);
        let mut canvas = surface.canvas();
        let mut small = canvas.sub(Rect::new(10, 10, 5, 5));
        small.text(-3, -3, "WWWWWW", WHITE);
        for y in 0..40 {
            for x in 0..40 {
                let inside = (10..15).contains(&x) && (10..15).contains(&y);
                if !inside {
                    assert_eq!(surface.pixel(x, y), Some(0), "({x}, {y})");
                }
            }
        }
        assert!(surface.pixels().iter().any(|&p| p != 0));
        // Far off-screen text costs nothing and breaks nothing.
        surface.canvas().text(i32::MAX - 5, 0, "abc", WHITE);
        surface.canvas().text(0, i32::MIN, "abc", WHITE);
    }

    #[test]
    fn blending_endpoints() {
        assert_eq!(blend(0x123456, 0xABCDEF, 255), 0x123456);
        assert_eq!(blend(0x123456, 0xABCDEF, 0), 0xABCDEF);
        assert_eq!(blend(0xFFFFFF, 0x000000, 128), 0x808080);
    }
}
