// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Drawing with soft edges: translucent fills, rounded rectangles with
//! smooth corners, soft shadows, proportional text and icons.
//!
//! Everything here blends a colour over what is already on the canvas,
//! by a coverage between 0 (leave the pixel alone) and 255 (replace it).
//! Shapes are described by how much of each pixel they cover, so their
//! edges come out smooth without drawing anything larger and scaling it
//! down.

use crate::Colour;
use crate::font::Font;
use crate::geometry::Rect;
use crate::icon::Mask;
use crate::surface::Canvas;

/// Mixes `over` onto `under`: `alpha` 0 leaves `under`, 255 gives `over`.
pub fn mix(over: Colour, under: Colour, alpha: u8) -> Colour {
    let alpha = u32::from(alpha);
    let channel = |shift: u32| {
        let o = (over >> shift) & 0xFF;
        let u = (under >> shift) & 0xFF;
        ((o * alpha + u * (255 - alpha) + 127) / 255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// `value * alpha / 255`, rounded: two coverages combined.
fn scale(value: u8, alpha: u8) -> u8 {
    ((u32::from(value) * u32::from(alpha) + 127) / 255) as u8
}

/// The integer square root: the largest `r` with `r * r <= value`.
fn isqrt(value: u64) -> u64 {
    if value < 2 {
        return value;
    }
    // Newton's iteration from above converges on the floor of the root.
    let mut guess = 1u64 << (value.ilog2() / 2 + 1);
    loop {
        let next = (guess + value / guess) / 2;
        if next >= guess {
            return guess;
        }
        guess = next;
    }
}

/// How far the centre of pixel (`x`, `y`) is outside the rectangle
/// `width` x `height` with corners rounded to `radius`, in 1/256 pixel;
/// negative inside. Exact near the corners and along the edges outside,
/// which is where it is needed.
fn distance(width: i32, height: i32, radius: i32, x: i32, y: i32) -> i32 {
    let radius = radius.clamp(0, width.min(height) / 2);
    // Twice the distance from the pixel's centre to the rectangle that
    // remains when the rounded border is taken off, per axis (whole
    // numbers, since centres are at half pixels).
    let beyond = |position: i32, size: i32| {
        let centre = 2 * position + 1;
        (2 * radius - centre)
            .max(centre - 2 * (size - radius))
            .max(0)
    };
    let (dx, dy) = (beyond(x, width), beyond(y, height));
    let outside = if dx == 0 || dy == 0 {
        (dx + dy) * 128
    } else {
        // sqrt((dx/2)^2 + (dy/2)^2) * 256
        isqrt((i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy)) as u64 * (1 << 14))
            as i32
    };
    if outside > 0 {
        return outside - radius * 256;
    }
    // Inside the inner rectangle: the nearest straight edge decides.
    let inset = |position: i32, size: i32| (2 * position + 1).min(2 * (size - position) - 1);
    -(inset(x, width).min(inset(y, height)) * 128)
}

/// How much of pixel (`x`, `y`) a rounded rectangle covers, 0–255.
fn rounded_coverage(width: i32, height: i32, radius: i32, x: i32, y: i32) -> u8 {
    // The edge passes through the pixel when its centre is within half a
    // pixel of it; coverage falls from all to nothing across that band.
    (128 - distance(width, height, radius, x, y)).clamp(0, 255) as u8
}

impl Canvas<'_> {
    /// Blends `colour` over each visible pixel of `area` by the coverage
    /// `shape` returns for it (given the pixel's position relative to
    /// `area`'s corner).
    pub fn cover(&mut self, area: Rect, colour: Colour, mut shape: impl FnMut(i32, i32) -> u8) {
        self.rows(area, |x, y, row| {
            for (offset, pixel) in row.iter_mut().enumerate() {
                match shape(x + offset as i32, y) {
                    0 => {}
                    255 => *pixel = colour,
                    alpha => *pixel = mix(colour, *pixel, alpha),
                }
            }
        });
    }

    /// Blends `colour` over `area` at `alpha`.
    pub fn tint(&mut self, area: Rect, colour: Colour, alpha: u8) {
        if alpha == 255 {
            self.fill(area, colour);
        } else {
            self.cover(area, colour, |_, _| alpha);
        }
    }

    /// Fills `area`, its corners rounded to `radius`, with `colour` at
    /// `alpha`.
    pub fn round_rect(&mut self, area: Rect, radius: i32, colour: Colour, alpha: u8) {
        let (width, height) = (area.w, area.h);
        if width <= 0 || height <= 0 {
            return;
        }
        let radius = radius.clamp(0, width.min(height) / 2);
        if radius == 0 {
            return self.tint(area, colour, alpha);
        }
        // Only the four corner squares have partly covered pixels; the
        // rest is three plain rectangles.
        self.tint(
            Rect::new(area.x, area.y + radius, width, height - 2 * radius),
            colour,
            alpha,
        );
        let across = width - 2 * radius;
        self.tint(
            Rect::new(area.x + radius, area.y, across, radius),
            colour,
            alpha,
        );
        self.tint(
            Rect::new(area.x + radius, area.bottom() - radius, across, radius),
            colour,
            alpha,
        );
        for (cx, cy) in [
            (0, 0),
            (width - radius, 0),
            (0, height - radius),
            (width - radius, height - radius),
        ] {
            self.cover(
                Rect::new(area.x + cx, area.y + cy, radius, radius),
                colour,
                |x, y| {
                    scale(
                        rounded_coverage(width, height, radius, cx + x, cy + y),
                        alpha,
                    )
                },
            );
        }
    }

    /// Draws a line one pixel wide just inside the edge of `area` with
    /// corners rounded to `radius`.
    pub fn round_outline(&mut self, area: Rect, radius: i32, colour: Colour, alpha: u8) {
        let (width, height) = (area.w, area.h);
        if width <= 0 || height <= 0 {
            return;
        }
        let radius = radius.clamp(0, width.min(height) / 2);
        // What the shape covers and the shape one pixel smaller does not.
        let line = move |x: i32, y: i32| {
            let outer = rounded_coverage(width, height, radius, x, y);
            let inner = if x >= 1 && y >= 1 && x < width - 1 && y < height - 1 {
                rounded_coverage(width - 2, height - 2, radius - 1, x - 1, y - 1)
            } else {
                0
            };
            scale(outer.saturating_sub(inner), alpha)
        };
        // The line lies within this many pixels of the edge.
        let margin = radius.max(1) + 1;
        if width <= 2 * margin || height <= 2 * margin {
            return self.cover(area, colour, line);
        }
        for strip in [
            Rect::new(0, 0, width, margin),
            Rect::new(0, height - margin, width, margin),
            Rect::new(0, margin, margin, height - 2 * margin),
            Rect::new(width - margin, margin, margin, height - 2 * margin),
        ] {
            self.cover(strip.translate(area.x, area.y), colour, |x, y| {
                line(strip.x + x, strip.y + y)
            });
        }
    }

    /// Draws the shadow that a rounded rectangle at `area` would cast:
    /// `alpha` at its edge, fading to nothing `blur` pixels out. Draw
    /// the object afterwards: the shadow also darkens a band just inside
    /// the rectangle's edge (so that no gap shows along it), but not its
    /// middle, which the object is going to cover anyway.
    pub fn shadow(&mut self, area: Rect, radius: i32, blur: i32, colour: Colour, alpha: u8) {
        let (width, height) = (area.w, area.h);
        if blur <= 0 || alpha == 0 || width <= 0 || height <= 0 {
            return;
        }
        let reach = blur * 256;
        let shade = move |x: i32, y: i32| {
            let outside = distance(width, height, radius, x, y);
            if outside <= 0 {
                alpha
            } else if outside >= reach {
                0
            } else {
                // Falls off as the square of the remaining distance: most
                // of the darkness is close to the object.
                let left = i64::from(reach - outside);
                ((i64::from(alpha) * left * left) / (i64::from(reach) * i64::from(reach))) as u8
            }
        };
        // Pixels this far inside are certainly covered by the object:
        // the shadow is a frame of four strips around the rest.
        let band = radius.clamp(2, (width.min(height) / 2).max(2));
        let strips = if width > 2 * band && height > 2 * band {
            [
                Rect::new(-blur, -blur, width + 2 * blur, blur + band),
                Rect::new(-blur, height - band, width + 2 * blur, blur + band),
                Rect::new(-blur, band, blur + band, height - 2 * band),
                Rect::new(width - band, band, blur + band, height - 2 * band),
            ]
        } else {
            // Too small to have a middle.
            [
                Rect::new(-blur, -blur, width + 2 * blur, height + 2 * blur),
                Rect::default(),
                Rect::default(),
                Rect::default(),
            ]
        };
        for strip in strips {
            self.cover(strip.translate(area.x, area.y), colour, |x, y| {
                shade(strip.x + x, strip.y + y)
            });
        }
    }

    /// Draws a square coverage mask (an icon) with its corner at
    /// (`x`, `y`) in `colour` at `alpha`.
    pub fn mask(&mut self, x: i32, y: i32, mask: Mask, colour: Colour, alpha: u8) {
        let size = mask.size;
        self.cover(Rect::new(x, y, size, size), colour, |mx, my| {
            scale(mask.coverage[(my * size + mx) as usize], alpha)
        });
    }

    /// Writes `text` in `font` and `colour`, the top of its line at `y`
    /// and its pen starting at `x`; returns where the pen ends up.
    pub fn write(&mut self, x: i32, y: i32, text: &str, font: Font, colour: Colour) -> i32 {
        let mut pen = x;
        let visible = self.visible();
        for character in text.chars() {
            let Some(glyph) = font.glyph_or_fallback(character) else {
                continue;
            };
            let cell = Rect::new(pen + glyph.left, y + glyph.top, glyph.width, glyph.height);
            if !cell.intersect(&visible).is_empty() {
                self.cover(cell, colour, |gx, gy| {
                    glyph.coverage[(gy * glyph.width + gx) as usize]
                });
            }
            pen += glyph.advance;
        }
        pen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::REGULAR_13;
    use crate::icon::icon;
    use crate::surface::Surface;

    const WHITE: Colour = 0xFF_FF_FF;
    const BLACK: Colour = 0x00_00_00;
    const BLUE: Colour = 0x32_6B_DF;

    #[test]
    fn mixing() {
        assert_eq!(mix(BLUE, WHITE, 0), WHITE);
        assert_eq!(mix(BLUE, WHITE, 255), BLUE);
        assert_eq!(mix(BLACK, WHITE, 128), 0x7F_7F_7F);
        // Each channel separately, without spilling into its neighbour.
        assert_eq!(mix(0xFF_00_00, 0x00_00_FF, 255), 0xFF_00_00);
        assert_eq!(mix(0xFF_00_FF, 0x00_FF_00, 51), 0x33_CC_33);
        for alpha in 0..=255 {
            assert_eq!(mix(0x12_34_56, 0x12_34_56, alpha), 0x12_34_56);
        }
    }

    #[test]
    fn square_roots() {
        for value in 0..5000u64 {
            let root = isqrt(value);
            assert!(
                root * root <= value && (root + 1) * (root + 1) > value,
                "{value}"
            );
        }
        assert_eq!(
            isqrt(u64::from(u32::MAX) * u64::from(u32::MAX)),
            u64::from(u32::MAX)
        );
    }

    #[test]
    fn a_rounded_rectangle_is_solid_inside_and_smooth_at_the_corners() {
        let mut surface = Surface::new(60, 40, WHITE);
        let area = Rect::new(10, 10, 40, 20);
        surface.canvas().round_rect(area, 8, BLACK, 255);
        let at = |x, y| surface.pixel(x, y).unwrap();
        // Centre, and the middles of all four edges just inside.
        for (x, y) in [(30, 20), (10, 20), (49, 20), (30, 10), (30, 29)] {
            assert_eq!(at(x, y), BLACK, "({x}, {y})");
        }
        // Outside, including the very corners of the bounding box.
        for (x, y) in [
            (9, 20),
            (50, 20),
            (30, 9),
            (30, 30),
            (10, 10),
            (49, 10),
            (10, 29),
            (49, 29),
        ] {
            assert_eq!(at(x, y), WHITE, "({x}, {y})");
        }
        // Along the corner's arc there are in-between shades.
        let greys = (10..18)
            .flat_map(|x| (10..18).map(move |y| (x, y)))
            .filter(|&(x, y)| at(x, y) != WHITE && at(x, y) != BLACK)
            .count();
        assert!(greys >= 6, "{greys} blended pixels");
        // The four corners are mirror images of each other.
        for x in 0..8 {
            for y in 0..8 {
                let corner = at(10 + x, 10 + y);
                assert_eq!(corner, at(49 - x, 10 + y));
                assert_eq!(corner, at(10 + x, 29 - y));
                assert_eq!(corner, at(49 - x, 29 - y));
            }
        }
        // The area covered is that of the shape: a rectangle less the
        // four corner squares plus a disc (π r²), within a pixel or two.
        let darkness: u32 = surface.pixels().iter().map(|&p| 255 - (p & 0xFF)).sum();
        let expected = 40.0 * 20.0 - (4.0 - core::f64::consts::PI) * 64.0;
        assert!(
            (f64::from(darkness) / 255.0 - expected).abs() < 2.0,
            "{darkness}"
        );
    }

    #[test]
    fn extreme_radii_and_sizes() {
        let mut surface = Surface::new(40, 40, WHITE);
        // A radius of half the side makes a disc; a larger one is the same.
        surface
            .canvas()
            .round_rect(Rect::new(4, 4, 20, 20), 10, BLACK, 255);
        let disc = surface.clone();
        let mut again = Surface::new(40, 40, WHITE);
        again
            .canvas()
            .round_rect(Rect::new(4, 4, 20, 20), 99, BLACK, 255);
        assert_eq!(disc, again);
        assert_eq!(disc.pixel(14, 14), Some(BLACK));
        assert_eq!(disc.pixel(4, 4), Some(WHITE));
        // Radius zero is an ordinary rectangle.
        let mut square = Surface::new(40, 40, WHITE);
        square
            .canvas()
            .round_rect(Rect::new(4, 4, 20, 20), 0, BLACK, 255);
        let mut filled = Surface::new(40, 40, WHITE);
        filled.canvas().fill(Rect::new(4, 4, 20, 20), BLACK);
        assert_eq!(square, filled);
        // Nothing and next to nothing.
        let mut empty = Surface::new(8, 8, WHITE);
        empty
            .canvas()
            .round_rect(Rect::new(2, 2, 0, 5), 3, BLACK, 255);
        empty
            .canvas()
            .round_rect(Rect::new(2, 2, 1, 1), 3, BLACK, 0);
        assert!(empty.pixels().iter().all(|&p| p == WHITE));
    }

    #[test]
    fn translucent_shapes_let_the_background_through() {
        let mut surface = Surface::new(20, 20, WHITE);
        surface.canvas().tint(Rect::new(0, 0, 10, 20), BLACK, 128);
        assert_eq!(surface.pixel(5, 5), Some(0x7F_7F_7F));
        assert_eq!(surface.pixel(15, 5), Some(WHITE));
        // A rounded shape at part strength is evenly that strong inside.
        surface
            .canvas()
            .round_rect(Rect::new(10, 0, 10, 10), 3, BLACK, 51);
        assert_eq!(surface.pixel(15, 5), Some(0xCC_CC_CC));
        // Full strength is a plain fill.
        surface.canvas().tint(Rect::new(0, 0, 20, 20), BLUE, 255);
        assert!(surface.pixels().iter().all(|&p| p == BLUE));
    }

    #[test]
    fn an_outline_is_one_pixel_wide() {
        let mut surface = Surface::new(40, 30, WHITE);
        surface
            .canvas()
            .round_outline(Rect::new(5, 5, 30, 20), 6, BLACK, 255);
        let at = |x, y| surface.pixel(x, y).unwrap();
        assert_eq!(at(20, 5), BLACK);
        assert_eq!(at(20, 6), WHITE);
        assert_eq!(at(20, 24), BLACK);
        assert_eq!(at(20, 23), WHITE);
        assert_eq!(at(5, 15), BLACK);
        assert_eq!(at(6, 15), WHITE);
        assert_eq!(at(34, 15), BLACK);
        assert_eq!(at(20, 15), WHITE);
        assert_eq!(at(5, 5), WHITE, "the corner itself is cut off");
        // About as much ink as the perimeter is long.
        let ink: u32 = surface.pixels().iter().map(|&p| 255 - (p & 0xFF)).sum();
        let perimeter = 2.0 * (30.0 + 20.0) - (8.0 - 2.0 * core::f64::consts::PI) * 6.0;
        assert!(
            (f64::from(ink) / 255.0 - perimeter).abs() < 8.0,
            "{}",
            ink / 255
        );
    }

    #[test]
    fn a_shadow_is_darkest_at_the_object_and_fades_out() {
        let mut surface = Surface::new(100, 100, WHITE);
        let area = Rect::new(30, 30, 40, 40);
        surface.canvas().shadow(area, 8, 20, BLACK, 100);
        let shade = |x, y| 255 - (surface.pixel(x, y).unwrap() & 0xFF);
        // Just inside the object's edge: full strength. Its middle is
        // left alone.
        assert_eq!(shade(31, 50), 100);
        assert_eq!(shade(50, 50), 0);
        // Fading to the left of it, monotonically, to nothing.
        let fade: std::vec::Vec<u32> = (9..=30).rev().map(|x| shade(x, 50)).collect();
        assert!(fade.windows(2).all(|pair| pair[0] >= pair[1]), "{fade:?}");
        assert!(fade[1] > 80 && fade[10] > 10 && fade[10] < 40);
        assert_eq!(shade(9, 50), 0);
        assert_eq!(shade(5, 50), 0);
        // The same in every direction.
        assert_eq!(shade(20, 50), shade(79, 50));
        assert_eq!(shade(20, 50), shade(50, 20));
        assert_eq!(shade(20, 50), shade(50, 79));
        // Rounder at the corners than along the edges: the box's corner
        // is farther from the object than the edge's middle.
        assert!(shade(22, 22) < shade(22, 50));
        // No blur, no shadow.
        let mut none = Surface::new(20, 20, WHITE);
        none.canvas()
            .shadow(Rect::new(5, 5, 10, 10), 2, 0, BLACK, 200);
        assert!(none.pixels().iter().all(|&p| p == WHITE));
    }

    #[test]
    fn text_is_written_proportionally_and_clipped() {
        let mut surface = Surface::new(120, 30, WHITE);
        let end = surface.canvas().write(4, 6, "Files", REGULAR_13, BLACK);
        assert_eq!(end, 4 + REGULAR_13.width("Files"));
        let ink = |surface: &Surface, x0: i32, x1: i32| {
            (x0..x1)
                .flat_map(|x| (0..30).map(move |y| (x, y)))
                .filter(|&(x, y)| surface.pixel(x, y) != Some(WHITE))
                .count()
        };
        assert!(ink(&surface, 4, end) > 40);
        assert_eq!(ink(&surface, end + 1, 120), 0);
        assert_eq!(ink(&surface, 0, 3), 0);
        // In colour: solid parts of letters are exactly that colour.
        let mut coloured = Surface::new(120, 30, WHITE);
        coloured.canvas().write(4, 6, "HHH", REGULAR_13, BLUE);
        assert!(coloured.pixels().contains(&BLUE));
        // Clipped: nothing outside the canvas given.
        let mut clipped = Surface::new(120, 30, WHITE);
        clipped.canvas().clipped(Rect::new(10, 0, 12, 30)).write(
            4,
            6,
            "Files and folders",
            REGULAR_13,
            BLACK,
        );
        assert!(ink(&clipped, 10, 22) > 5);
        assert_eq!(ink(&clipped, 0, 10) + ink(&clipped, 22, 120), 0);
        // Written partly or wholly off the surface: no panic.
        let mut canvas = clipped.canvas();
        canvas.write(-30, -5, "off the edge", REGULAR_13, BLACK);
        canvas.write(110, 25, "off the edge", REGULAR_13, BLACK);
        canvas.write(500, 500, "nowhere", REGULAR_13, BLACK);
    }

    #[test]
    fn icons_are_drawn_in_any_colour() {
        let mut surface = Surface::new(30, 30, WHITE);
        let close = icon("close", 16).unwrap();
        surface.canvas().mask(7, 7, close, 0xD9_3A_30, 255);
        let red = surface.pixels().iter().filter(|&&p| p != WHITE).count();
        assert!(red > 20 && red < 120, "{red}");
        // Nothing outside the icon's square.
        for (x, y) in [(6, 15), (23, 15), (15, 6), (15, 23)] {
            assert_eq!(surface.pixel(x, y), Some(WHITE));
        }
        // At no strength nothing is drawn; off the edge nothing breaks.
        let mut blank = Surface::new(30, 30, WHITE);
        blank.canvas().mask(7, 7, close, BLACK, 0);
        blank.canvas().mask(-40, -40, close, BLACK, 255);
        assert!(blank.pixels().iter().all(|&p| p == WHITE));
        blank.canvas().mask(-8, 22, close, BLACK, 255);
    }

    /// The shapes drawn the slow, obvious way: one closure over the
    /// whole area.
    fn reference(canvas: &mut Canvas<'_>, kind: u32, area: Rect, radius: i32, alpha: u8) {
        let (width, height) = (area.w, area.h);
        let radius_in = radius.clamp(0, width.min(height).max(0) / 2);
        match kind {
            0 => canvas.cover(area, BLUE, |x, y| {
                scale(rounded_coverage(width, height, radius_in, x, y), alpha)
            }),
            1 => canvas.cover(area, BLUE, |x, y| {
                let outer = rounded_coverage(width, height, radius_in, x, y);
                let inner = if x >= 1 && y >= 1 && x < width - 1 && y < height - 1 {
                    rounded_coverage(width - 2, height - 2, radius_in - 1, x - 1, y - 1)
                } else {
                    0
                };
                scale(outer.saturating_sub(inner), alpha)
            }),
            _ => {
                let blur = 9;
                let band = radius.clamp(2, (width.min(height) / 2).max(2));
                let around = Rect::new(area.x - blur, area.y - blur, width + 18, height + 18);
                canvas.cover(around, BLUE, |x, y| {
                    let (x, y) = (x - blur, y - blur);
                    let middle = width > 2 * band
                        && height > 2 * band
                        && x >= band
                        && x < width - band
                        && y >= band
                        && y < height - band;
                    if middle {
                        return 0;
                    }
                    let outside = distance(width, height, radius, x, y);
                    if outside <= 0 {
                        alpha
                    } else if outside >= blur * 256 {
                        0
                    } else {
                        let left = i64::from(blur * 256 - outside);
                        ((i64::from(alpha) * left * left) / (i64::from(blur * 256).pow(2))) as u8
                    }
                });
            }
        }
    }

    #[test]
    fn the_fast_paths_draw_exactly_what_the_obvious_way_does() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |limit: i32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % limit as u64) as i32
        };
        for round in 0..600 {
            let area = Rect::new(next(40) - 5, next(40) - 5, next(60) + 1, next(60) + 1);
            let radius = next(20);
            let alpha = [255, 255, 128, 31, 1][next(5) as usize];
            let kind = (round % 3) as u32;
            let mut fast = Surface::new(80, 80, WHITE);
            let mut slow = Surface::new(80, 80, WHITE);
            // Through a clip, as the desktop draws: part of the shape only.
            let clip = Rect::new(next(20), next(20), 40 + next(40), 40 + next(40));
            {
                let mut canvas = fast.canvas();
                let mut canvas = canvas.clipped(clip);
                match kind {
                    0 => canvas.round_rect(area, radius, BLUE, alpha),
                    1 => canvas.round_outline(area, radius, BLUE, alpha),
                    _ => canvas.shadow(area, radius, 9, BLUE, alpha),
                }
            }
            {
                let mut canvas = slow.canvas();
                let mut canvas = canvas.clipped(clip);
                reference(&mut canvas, kind, area, radius, alpha);
            }
            assert!(
                fast == slow,
                "kind {kind}, {area:?}, radius {radius}, alpha {alpha}"
            );
        }
    }
}
