// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The controls of the Omit OS design system, for windows to draw: a
//! segmented control, a colour swatch, a card, a switch.
//!
//! A control here is two functions: one that says where its parts are
//! (so that a click can be matched to a part), and one that draws it.
//! Windows keep the state — which segment is chosen, whether a switch is
//! on — themselves.

use alloc::vec::Vec;

use crate::Colour;
use crate::font::MEDIUM_13;
use crate::geometry::{Point, Rect};
use crate::surface::Canvas;
use crate::theme::Theme;

/// Height of a segmented control.
pub const SEGMENTED_HEIGHT: i32 = 32;
/// Side of a colour swatch, ring included.
pub const SWATCH: i32 = 38;
/// Size of a switch.
pub const SWITCH: (i32, i32) = (36, 20);

/// Where the segments of a segmented control with these labels are, the
/// control's top-left corner being (`x`, `y`): one rectangle per label,
/// left to right.
pub fn segments(x: i32, y: i32, labels: &[&str]) -> Vec<Rect> {
    let mut left = x + 2;
    labels
        .iter()
        .map(|label| {
            let width = MEDIUM_13.width(label) + 32;
            let segment = Rect::new(left, y + 2, width, SEGMENTED_HEIGHT - 4);
            left += width + 2;
            segment
        })
        .collect()
}

/// Which segment of that control `at` is in, if any.
pub fn segment_at(x: i32, y: i32, labels: &[&str], at: Point) -> Option<usize> {
    segments(x, y, labels)
        .iter()
        .position(|segment| segment.contains(at))
}

/// Draws a segmented control — a row of choices of which one, `selected`,
/// is in force — and returns the area it takes.
pub fn segmented(
    canvas: &mut Canvas<'_>,
    x: i32,
    y: i32,
    labels: &[&str],
    selected: usize,
    theme: &Theme,
) -> Rect {
    let parts = segments(x, y, labels);
    let right = parts.last().map_or(x + 4, |last| last.right() + 2);
    let whole = Rect::new(x, y, right - x, SEGMENTED_HEIGHT);
    canvas.round_rect(whole, 8, theme.fill_2.colour, theme.fill_2.alpha);
    for (index, (segment, label)) in parts.iter().zip(labels).enumerate() {
        let ink = if index == selected {
            // The chosen segment is a raised key.
            canvas.shadow(segment.translate(0, 1), 6, 3, theme.shadow, 30);
            canvas.round_rect(*segment, 6, theme.control, 255);
            canvas.round_outline(*segment, 6, theme.separator.colour, theme.separator.alpha);
            theme.text
        } else {
            theme.text_2
        };
        canvas.write(
            segment.x + 16,
            segment.y + (segment.h - MEDIUM_13.height()) / 2,
            label,
            MEDIUM_13,
            ink,
        );
    }
    whole
}

/// Draws a round colour swatch in the square `SWATCH` wide at (`x`,
/// `y`); the selected one has a ring around it. `surface` is the colour
/// it is drawn on.
pub fn swatch(
    canvas: &mut Canvas<'_>,
    x: i32,
    y: i32,
    colour: Colour,
    selected: bool,
    surface: Colour,
    theme: &Theme,
) {
    let whole = Rect::new(x, y, SWATCH, SWATCH);
    if selected {
        // A ring in the text colour, then a gap, then the colour.
        canvas.round_rect(whole, SWATCH / 2, theme.text, 255);
        canvas.round_rect(whole.inset(2), SWATCH / 2, surface, 255);
    }
    canvas.round_rect(whole.inset(4), SWATCH / 2, colour, 255);
}

/// Draws a card: a raised, rounded area that groups rows of settings.
pub fn card(canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
    canvas.round_rect(area, 12, theme.window_raised, 255);
    canvas.round_outline(area, 12, theme.separator.colour, theme.separator.alpha);
}

/// Draws a switch, on or off, with its top-left corner at (`x`, `y`).
pub fn switch(canvas: &mut Canvas<'_>, x: i32, y: i32, on: bool, theme: &Theme) {
    let track = Rect::new(x, y, SWITCH.0, SWITCH.1);
    if on {
        canvas.round_rect(track, SWITCH.1 / 2, theme.accent, 255);
    } else {
        canvas.round_rect(
            track,
            SWITCH.1 / 2,
            theme.fill_2.colour,
            theme.fill_2.alpha.saturating_mul(2),
        );
    }
    let knob_x = if on {
        x + SWITCH.0 - SWITCH.1 + 2
    } else {
        x + 2
    };
    let knob = Rect::new(knob_x, y + 2, SWITCH.1 - 4, SWITCH.1 - 4);
    canvas.shadow(knob.translate(0, 1), knob.w / 2, 2, theme.shadow, 40);
    canvas.round_rect(knob, knob.w / 2, 0xFF_FF_FF, 255);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Surface;
    use crate::theme::{Accent, Mode};

    #[test]
    fn segments_are_side_by_side_and_as_wide_as_their_labels() {
        let labels = ["Light", "Dark", "A much longer label"];
        let parts = segments(10, 20, &labels);
        assert_eq!(parts.len(), 3);
        assert_eq!((parts[0].x, parts[0].y, parts[0].h), (12, 22, 28));
        for pair in parts.windows(2) {
            assert_eq!(pair[1].x, pair[0].right() + 2);
        }
        assert!(parts[2].w > parts[0].w);
        assert_eq!(parts[0].w, MEDIUM_13.width("Light") + 32);
        // Clicks: each segment's middle is that segment; the gaps, the
        // padding and everything outside are nothing.
        for (index, part) in parts.iter().enumerate() {
            let middle = Point::new(part.x + part.w / 2, part.y + part.h / 2);
            assert_eq!(segment_at(10, 20, &labels, middle), Some(index));
        }
        assert_eq!(segment_at(10, 20, &labels, Point::new(11, 30)), None);
        assert_eq!(
            segment_at(10, 20, &labels, Point::new(parts[0].right(), 30)),
            None
        );
        assert_eq!(segment_at(10, 20, &labels, Point::new(40, 19)), None);
        assert!(segments(0, 0, &[]).is_empty());
    }

    #[test]
    fn the_chosen_segment_stands_out() {
        for mode in [Mode::Light, Mode::Dark] {
            let theme = Theme::new(mode, Accent::Blue);
            let labels = ["Left", "Centre"];
            let draw = |selected| {
                let mut surface = Surface::new(200, 60, theme.window);
                let area = segmented(&mut surface.canvas(), 10, 10, &labels, selected, &theme);
                (surface, area)
            };
            let (first, area) = draw(0);
            let (second, _) = draw(1);
            let parts = segments(10, 10, &labels);
            assert_eq!(area, Rect::new(10, 10, parts[1].right() + 2 - 10, 32));
            // The chosen segment is filled with the control colour.
            assert_eq!(
                first.pixel(parts[0].x + 4, parts[0].y + 4),
                Some(theme.control)
            );
            assert_ne!(
                first.pixel(parts[1].x + 4, parts[1].y + 4),
                Some(theme.control)
            );
            assert_eq!(
                second.pixel(parts[1].x + 4, parts[1].y + 4),
                Some(theme.control)
            );
            // Nothing is drawn outside the control (but for the key's
            // faint shadow, which stays within a few pixels).
            let around = area.inset(-5);
            for y in 0..60 {
                for x in 0..200 {
                    if !around.contains(Point::new(x, y)) {
                        assert_eq!(first.pixel(x, y), Some(theme.window), "({x}, {y})");
                    }
                }
            }
        }
    }

    #[test]
    fn swatches_are_discs_and_the_selected_one_has_a_ring() {
        let theme = Theme::default();
        let mut surface = Surface::new(100, 50, theme.window);
        let colour = Accent::Rose.colour();
        swatch(
            &mut surface.canvas(),
            5,
            5,
            colour,
            false,
            theme.window,
            &theme,
        );
        swatch(
            &mut surface.canvas(),
            55,
            5,
            colour,
            true,
            theme.window,
            &theme,
        );
        let at = |x, y| surface.pixel(x, y).unwrap();
        // Centres are the colour; the squares' corners are untouched.
        assert_eq!(at(5 + 19, 5 + 19), colour);
        assert_eq!(at(55 + 19, 5 + 19), colour);
        assert_eq!(at(6, 6), theme.window);
        assert_eq!(at(56, 6), theme.window);
        // Along the middle row: ring, gap, colour for the selected one;
        // nothing, then colour for the other.
        assert_eq!(at(55, 5 + 19), theme.text);
        assert_eq!(at(55 + 3, 5 + 19), theme.window);
        assert_eq!(at(55 + 6, 5 + 19), colour);
        assert_eq!(at(5 + 1, 5 + 19), theme.window);
        assert_eq!(at(5 + 6, 5 + 19), colour);
    }

    #[test]
    fn a_switch_shows_its_state() {
        let theme = Theme::new(Mode::Light, Accent::Teal);
        let mut surface = Surface::new(100, 30, theme.window);
        switch(&mut surface.canvas(), 5, 5, false, &theme);
        switch(&mut surface.canvas(), 55, 5, true, &theme);
        let at = |x, y| surface.pixel(x, y).unwrap();
        // Off: knob at the left, track not in the accent.
        assert_eq!(at(5 + 10, 5 + 10), 0xFF_FF_FF);
        assert_ne!(at(5 + 28, 5 + 10), theme.accent);
        assert_ne!(at(5 + 28, 5 + 10), theme.window);
        // On: knob at the right, track in the accent.
        assert_eq!(at(55 + 26, 5 + 10), 0xFF_FF_FF);
        assert_eq!(at(55 + 8, 5 + 10), theme.accent);
    }

    #[test]
    fn a_card_is_a_raised_rounded_area() {
        let theme = Theme::default();
        let mut surface = Surface::new(120, 80, theme.window);
        card(&mut surface.canvas(), Rect::new(10, 10, 100, 60), &theme);
        assert_eq!(surface.pixel(60, 40), Some(theme.window_raised));
        assert_eq!(surface.pixel(10, 10), Some(theme.window), "rounded corner");
        assert_eq!(surface.pixel(5, 40), Some(theme.window));
        // An edge line.
        assert_eq!(
            surface.pixel(60, 10),
            Some(theme.separator.over(theme.window_raised))
        );
    }
}
