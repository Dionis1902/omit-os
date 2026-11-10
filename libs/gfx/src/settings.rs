// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Settings window: "Look & feel".
//!
//! Three choices, each applied the moment it is made: light or dark
//! mode, the accent colour, and where the programs' icons sit on the
//! task bar. The window lives in this library, beside the window
//! manager it configures, so that both are tested together.
//!
//! The design system's Settings has many more pages; this is the one
//! whose every control does something today.

use crate::desktop::{Alignment, App, Context};
use crate::font::{REGULAR_12, REGULAR_13, SEMIBOLD_13, SEMIBOLD_20};
use crate::geometry::{Point, Rect};
use crate::surface::Canvas;
use crate::theme::{Accent, Mode, Theme};
use crate::widgets;

/// The size of the window's content area; it is not resizable.
pub const SIZE: (i32, i32) = (520, 372);

const PADDING: i32 = 24;
const MODES: [&str; 2] = ["Light", "Dark"];
const ALIGNMENTS: [&str; 2] = ["Left", "Centre"];
/// Tops of the two cards, and the height of a row in them.
const APPEARANCE: i32 = 92;
const ROW: i32 = 64;
const TASKBAR: i32 = APPEARANCE + 2 * ROW + 50;

/// What Settings chooses, in a form that can be kept in a file:
/// three lines, `mode=dark`, `accent=rose`, `alignment=centre`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Saved {
    /// Light or dark.
    pub mode: Mode,
    /// The accent colour.
    pub accent: Accent,
    /// Where the task bar's icons sit.
    pub alignment: Alignment,
}

impl Saved {
    /// Reads what [`Saved::to_text`] wrote. Lines that are missing,
    /// unknown or damaged leave their setting at its default: a file
    /// from another version, or a broken one, never stops the desktop.
    pub fn parse(text: &str) -> Self {
        let mut saved = Self::default();
        for line in text.lines() {
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            match (name.trim(), value.trim()) {
                ("mode", "light") => saved.mode = Mode::Light,
                ("mode", "dark") => saved.mode = Mode::Dark,
                ("alignment", "left") => saved.alignment = Alignment::Left,
                ("alignment", "centre") => saved.alignment = Alignment::Centre,
                ("accent", value) => {
                    let named = Accent::ALL
                        .into_iter()
                        .find(|accent| accent.name().eq_ignore_ascii_case(value));
                    if let Some(accent) = named {
                        saved.accent = accent;
                    }
                }
                _ => {}
            }
        }
        saved
    }

    /// The settings as text for a file.
    pub fn to_text(&self) -> alloc::string::String {
        alloc::format!(
            "mode={}\naccent={}\nalignment={}\n",
            match self.mode {
                Mode::Light => "light",
                Mode::Dark => "dark",
            },
            self.accent.name().to_ascii_lowercase(),
            match self.alignment {
                Alignment::Left => "left",
                Alignment::Centre => "centre",
            },
        )
    }
}

/// Something in the window that can be clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Light or dark.
    Mode(Mode),
    /// An accent colour.
    Accent(Accent),
    /// Where the task bar's icons sit.
    Alignment(Alignment),
}

/// Where in the content area the control for `choice` is.
pub fn control(choice: Choice) -> Rect {
    let right = SIZE.0 - PADDING - 16;
    let width_of = |labels: &[&str]| {
        widgets::segments(0, 0, labels)
            .last()
            .map_or(0, |last| last.right() + 2)
    };
    match choice {
        Choice::Mode(mode) => {
            widgets::segments(right - width_of(&MODES), APPEARANCE + 16, &MODES)[mode as usize]
        }
        Choice::Alignment(alignment) => {
            widgets::segments(right - width_of(&ALIGNMENTS), TASKBAR + 16, &ALIGNMENTS)
                [alignment as usize]
        }
        Choice::Accent(accent) => {
            let index = Accent::ALL.iter().position(|&a| a == accent).unwrap_or(0) as i32;
            let first = right - Accent::ALL.len() as i32 * widgets::SWATCH;
            Rect::new(
                first + index * widgets::SWATCH,
                APPEARANCE + ROW + 13,
                widgets::SWATCH,
                widgets::SWATCH,
            )
        }
    }
}

/// Every choice the window offers.
fn choices() -> impl Iterator<Item = Choice> {
    [Mode::Light, Mode::Dark]
        .into_iter()
        .map(Choice::Mode)
        .chain(Accent::ALL.into_iter().map(Choice::Accent))
        .chain(
            [Alignment::Left, Alignment::Centre]
                .into_iter()
                .map(Choice::Alignment),
        )
}

/// The window's contents. What is in force — mode, accent, alignment —
/// it learns from the desktop each time it is drawn.
#[derive(Default)]
pub struct Settings {
    alignment: Alignment,
}

impl Settings {
    /// The window.
    pub fn new() -> Self {
        Self::default()
    }
}

impl App for Settings {
    fn title(&self) -> &str {
        "Settings"
    }

    fn icon(&self) -> &str {
        "settings"
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, _height: i32, theme: &Theme) {
        canvas.write(PADDING, 20, "Look & feel", SEMIBOLD_20, theme.text);
        let row = |canvas: &mut Canvas<'_>, y: i32, title: &str, hint: &str| {
            canvas.write(PADDING + 16, y + 14, title, REGULAR_13, theme.text);
            canvas.write(PADDING + 16, y + 34, hint, REGULAR_12, theme.text_3);
        };
        let corner = |choice| {
            let area = control(choice);
            // (A segment lies two pixels inside its control.)
            (area.x - 2, area.y - 2)
        };

        canvas.write(
            PADDING,
            APPEARANCE - 26,
            "Appearance",
            SEMIBOLD_13,
            theme.text_2,
        );
        let card = Rect::new(PADDING, APPEARANCE, width - 2 * PADDING, 2 * ROW);
        widgets::card(canvas, card, theme);
        row(canvas, card.y, "Mode", "Light or dark surfaces");
        let (x, y) = corner(Choice::Mode(Mode::Light));
        widgets::segmented(canvas, x, y, &MODES, theme.mode as usize, theme);
        canvas.tint(
            Rect::new(card.x + 16, card.y + ROW, card.w - 32, 1),
            theme.separator.colour,
            theme.separator.alpha,
        );
        row(
            canvas,
            card.y + ROW,
            "Accent colour",
            "Marks what is selected",
        );
        for accent in Accent::ALL {
            let area = control(Choice::Accent(accent));
            widgets::swatch(
                canvas,
                area.x,
                area.y,
                accent.colour(),
                accent == theme.accent_name,
                theme.window_raised,
                theme,
            );
        }

        canvas.write(PADDING, TASKBAR - 26, "Task bar", SEMIBOLD_13, theme.text_2);
        let card = Rect::new(PADDING, TASKBAR, width - 2 * PADDING, ROW);
        widgets::card(canvas, card, theme);
        row(canvas, card.y, "Alignment", "Where the programs' icons sit");
        let (x, y) = corner(Choice::Alignment(Alignment::Left));
        widgets::segmented(canvas, x, y, &ALIGNMENTS, self.alignment as usize, theme);
    }

    fn click(&mut self, x: i32, y: i32, context: &mut Context) {
        let at = Point::new(x, y);
        match choices().find(|&choice| control(choice).contains(at)) {
            Some(Choice::Mode(mode)) => context.set_mode(mode),
            Some(Choice::Accent(accent)) => context.set_accent(accent),
            Some(Choice::Alignment(alignment)) => {
                self.alignment = alignment;
                context.set_alignment(alignment);
            }
            None => {}
        }
    }

    fn resizable(&self) -> bool {
        false
    }

    fn alignment(&mut self, alignment: Alignment) {
        self.alignment = alignment;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::Desktop;
    use crate::surface::Surface;
    use alloc::boxed::Box;

    const W: usize = 800;
    const H: usize = 600;

    struct Fixture {
        desktop: Desktop,
        surface: Surface,
        /// The content area's corner on the screen.
        origin: Point,
    }

    impl Fixture {
        fn new() -> Self {
            let mut desktop = Desktop::new(W, H);
            desktop.add_launcher("Files", "drive", |_| {});
            let id = desktop.open(Box::new(Settings::new()), SIZE.0, SIZE.1);
            let frame = desktop.window_frame(id).unwrap();
            Self {
                desktop,
                surface: Surface::new(W, H, 0),
                origin: Point::new(
                    frame.x + crate::desktop::BORDER,
                    frame.y + crate::desktop::BORDER + crate::desktop::TITLE_HEIGHT,
                ),
            }
        }

        fn click(&mut self, choice: Choice) {
            let area = control(choice);
            let (x, y) = (
                self.origin.x + area.x + area.w / 2,
                self.origin.y + area.y + area.h / 2,
            );
            let at = self.desktop.pointer();
            self.desktop.mouse(x - at.x, y - at.y, 0);
            self.desktop.mouse(0, 0, 1);
            self.desktop.mouse(0, 0, 0);
        }

        fn pixel_in(&mut self, x: i32, y: i32) -> u32 {
            // (With the pointer out of the way.)
            let at = self.desktop.pointer();
            self.desktop.mouse(780 - at.x, 20 - at.y, 0);
            self.desktop.render(&mut self.surface);
            self.surface
                .pixel(self.origin.x + x, self.origin.y + y)
                .unwrap()
        }
    }

    #[test]
    fn the_controls_fit_the_window_and_do_not_overlap() {
        let all: alloc::vec::Vec<(Choice, Rect)> =
            choices().map(|choice| (choice, control(choice))).collect();
        assert_eq!(all.len(), 2 + 6 + 2);
        let content = Rect::new(0, 0, SIZE.0, SIZE.1);
        for (index, (choice, area)) in all.iter().enumerate() {
            assert_eq!(
                area.intersect(&content),
                *area,
                "{choice:?} is inside the window"
            );
            for (other, other_area) in &all[index + 1..] {
                assert!(
                    area.intersect(other_area).is_empty(),
                    "{choice:?} and {other:?}"
                );
            }
        }
    }

    #[test]
    fn choosing_a_mode_or_an_accent_changes_the_whole_desktop_at_once() {
        let mut f = Fixture::new();
        assert_eq!(f.desktop.theme().mode, Mode::Light);
        let light_window = f.desktop.theme().window;
        assert_eq!(f.pixel_in(250, 60), light_window);

        f.click(Choice::Mode(Mode::Dark));
        let theme = *f.desktop.theme();
        assert_eq!((theme.mode, theme.accent_name), (Mode::Dark, Accent::Blue));
        assert_eq!(f.pixel_in(250, 60), theme.window);
        assert_ne!(theme.window, light_window);
        // The accent changes and the mode stays.
        f.click(Choice::Accent(Accent::Rose));
        let theme = *f.desktop.theme();
        assert_eq!((theme.mode, theme.accent_name), (Mode::Dark, Accent::Rose));
        // And back to light, keeping the accent.
        f.click(Choice::Mode(Mode::Light));
        let theme = *f.desktop.theme();
        assert_eq!((theme.mode, theme.accent_name), (Mode::Light, Accent::Rose));
        for accent in Accent::ALL {
            f.click(Choice::Accent(accent));
            assert_eq!(f.desktop.theme().accent_name, accent);
        }
    }

    #[test]
    fn the_window_shows_what_is_in_force() {
        let mut f = Fixture::new();
        // The chosen segment is filled with the control colour.
        let inside = |choice| {
            let area = control(choice);
            (area.x + 4, area.y + 4)
        };
        let (lx, ly) = inside(Choice::Mode(Mode::Light));
        let (dx, dy) = inside(Choice::Mode(Mode::Dark));
        let theme = *f.desktop.theme();
        assert_eq!(f.pixel_in(lx, ly), theme.control);
        assert_ne!(f.pixel_in(dx, dy), theme.control);
        f.click(Choice::Mode(Mode::Dark));
        let theme = *f.desktop.theme();
        assert_eq!(f.pixel_in(dx, dy), theme.control);
        assert_ne!(f.pixel_in(lx, ly), theme.control);
        // The chosen accent has a ring: its swatch's left edge is in the
        // text colour, the others' are not.
        let edge = |accent| {
            let area = control(Choice::Accent(accent));
            (area.x, area.y + area.h / 2)
        };
        let (bx, by) = edge(Accent::Blue);
        let (tx, ty) = edge(Accent::Teal);
        assert_eq!(f.pixel_in(bx, by), theme.text);
        assert_ne!(f.pixel_in(tx, ty), theme.text);
        f.click(Choice::Accent(Accent::Teal));
        let theme = *f.desktop.theme();
        assert_eq!(f.pixel_in(tx, ty), theme.text);
        assert_ne!(f.pixel_in(bx, by), theme.text);
    }

    #[test]
    fn the_task_bar_alignment_follows() {
        let mut f = Fixture::new();
        let left = f.desktop.launcher_frame(0).unwrap();
        f.click(Choice::Alignment(Alignment::Centre));
        assert_eq!(f.desktop.alignment(), Alignment::Centre);
        let centre = f.desktop.launcher_frame(0).unwrap();
        assert!(centre.x > left.x + 100);
        let (cx, cy) = {
            let area = control(Choice::Alignment(Alignment::Centre));
            (area.x + 4, area.y + 4)
        };
        let theme = *f.desktop.theme();
        assert_eq!(f.pixel_in(cx, cy), theme.control);
        f.click(Choice::Alignment(Alignment::Left));
        assert_eq!(f.desktop.launcher_frame(0), Some(left));
    }

    #[test]
    fn a_window_opened_later_shows_the_alignment_in_force() {
        let mut f = Fixture::new();
        f.click(Choice::Alignment(Alignment::Centre));
        // A second Settings window, on top of the first.
        let id = f.desktop.open(Box::new(Settings::new()), SIZE.0, SIZE.1);
        let frame = f.desktop.window_frame(id).unwrap();
        f.origin = Point::new(
            frame.x + crate::desktop::BORDER,
            frame.y + crate::desktop::BORDER + crate::desktop::TITLE_HEIGHT,
        );
        let area = control(Choice::Alignment(Alignment::Centre));
        let theme = *f.desktop.theme();
        assert_eq!(f.pixel_in(area.x + 4, area.y + 4), theme.control);
    }

    #[test]
    fn settings_survive_being_written_to_a_file_and_read_back() {
        for mode in [Mode::Light, Mode::Dark] {
            for accent in Accent::ALL {
                for alignment in [Alignment::Left, Alignment::Centre] {
                    let saved = Saved {
                        mode,
                        accent,
                        alignment,
                    };
                    assert_eq!(Saved::parse(&saved.to_text()), saved);
                }
            }
        }
        assert_eq!(
            Saved {
                mode: Mode::Dark,
                accent: Accent::Rose,
                alignment: Alignment::Centre
            }
            .to_text(),
            "mode=dark\naccent=rose\nalignment=centre\n"
        );
    }

    #[test]
    fn a_damaged_or_foreign_settings_file_gives_the_defaults() {
        assert_eq!(Saved::parse(""), Saved::default());
        assert_eq!(
            Saved::parse("\u{0}\u{1}garbage\n===\n=\nmode\n"),
            Saved::default()
        );
        // What is understood is used; the rest is ignored.
        let partly =
            Saved::parse(" mode = dark \nwallpaper=dawn\naccent=ultraviolet\nalignment=middle\r\n");
        assert_eq!(
            partly,
            Saved {
                mode: Mode::Dark,
                ..Saved::default()
            }
        );
        // The last of several lines for one setting counts; case of the
        // accent's name does not matter.
        let twice = Saved::parse("accent=teal\naccent=AMBER\n");
        assert_eq!(twice.accent, Accent::Amber);
    }

    #[test]
    fn clicks_beside_the_controls_change_nothing() {
        let mut f = Fixture::new();
        let before = *f.desktop.theme();
        for (x, y) in [(10, 10), (40, 110), (250, 200), (500, 360), (30, 300)] {
            let at = f.desktop.pointer();
            f.desktop
                .mouse(f.origin.x + x - at.x, f.origin.y + y - at.y, 0);
            f.desktop.mouse(0, 0, 1);
            f.desktop.mouse(0, 0, 0);
        }
        assert_eq!(*f.desktop.theme(), before);
        assert_eq!(f.desktop.alignment(), Alignment::Left);
    }
}
