// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The colours of the interface: the tokens of the Omit OS design
//! system, for its light and dark modes and its six accent colours.
//!
//! Many tokens are translucent black or white — a separator is "black at
//! 8%" in light mode and "white at 8%" in dark — so that they work on any
//! surface. Those are [`Tone`]s, drawn with [`Canvas::tone`] and friends;
//! opaque colours are plain [`Colour`]s.
//!
//! [`Canvas::tone`]: crate::surface::Canvas

use crate::Colour;
use crate::paint::mix;

/// A colour and how strongly to blend it (0–255).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tone {
    /// The colour.
    pub colour: Colour,
    /// Its strength: 255 is opaque.
    pub alpha: u8,
}

impl Tone {
    const fn new(colour: Colour, percent_times_ten: u32) -> Self {
        Self {
            colour,
            alpha: ((percent_times_ten * 255 + 500) / 1000) as u8,
        }
    }

    /// What this tone looks like on `background`.
    pub fn over(self, background: Colour) -> Colour {
        mix(self.colour, background, self.alpha)
    }
}

/// Light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Dark text on light surfaces.
    #[default]
    Light,
    /// Light text on dark surfaces.
    Dark,
}

/// The colour that marks what is selected, active or the main action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accent {
    /// The default.
    #[default]
    Blue,
    /// Violet.
    Violet,
    /// Teal.
    Teal,
    /// Rose.
    Rose,
    /// Amber.
    Amber,
    /// Graphite.
    Graphite,
}

impl Accent {
    /// All six, in the order Settings shows them.
    pub const ALL: [Self; 6] = [
        Self::Blue,
        Self::Violet,
        Self::Teal,
        Self::Rose,
        Self::Amber,
        Self::Graphite,
    ];

    /// The accent colour itself.
    pub const fn colour(self) -> Colour {
        match self {
            Self::Blue => 0x32_6B_DF,
            Self::Violet => 0x71_52_E8,
            Self::Teal => 0x0B_7F_80,
            Self::Rose => 0xC9_3A_63,
            Self::Amber => 0xA8_58_00,
            Self::Graphite => 0x5F_66_70,
        }
    }

    /// Its name, as Settings shows it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Violet => "Violet",
            Self::Teal => "Teal",
            Self::Rose => "Rose",
            Self::Amber => "Amber",
            Self::Graphite => "Graphite",
        }
    }
}

const BLACK: Colour = 0x00_00_00;
const WHITE: Colour = 0xFF_FF_FF;

/// Every colour the interface draws with, for one mode and accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Light or dark.
    pub mode: Mode,
    /// Which accent.
    pub accent_name: Accent,
    /// The accent colour: fills of selected and primary things.
    pub accent: Colour,
    /// The accent as a colour for text and small marks (lightened in
    /// dark mode so that it stays readable).
    pub accent_text: Colour,
    /// Text on an accent fill.
    pub on_accent: Colour,
    /// The accent as a soft fill behind selected rows.
    pub accent_soft: Tone,
    /// Window contents.
    pub window: Colour,
    /// Raised areas within a window.
    pub window_raised: Colour,
    /// Title bars and sidebars.
    pub chrome: Colour,
    /// Controls that stand out from the window: the selected segment of
    /// a segmented control, a text field.
    pub control: Colour,
    /// Primary text.
    pub text: Colour,
    /// Secondary text.
    pub text_2: Colour,
    /// Tertiary text: hints, shortcuts, inactive titles.
    pub text_3: Colour,
    /// Hairlines between things.
    pub separator: Tone,
    /// Quiet fills: the window-control capsule, chips.
    pub fill: Tone,
    /// Stronger fills: a hovered control, a selected sidebar row.
    pub fill_2: Tone,
    /// Behind something the pointer is over.
    pub hover: Tone,
    /// Behind something being pressed.
    pub press: Tone,
    /// The task bar, over whatever is behind it.
    pub bar: Tone,
    /// Menus and panels, over whatever is behind them.
    pub glass: Tone,
    /// The light edge along the top of glass surfaces.
    pub highlight: Tone,
    /// The dot under a running program's task bar icon.
    pub running: Tone,
    /// Destructive actions; the close button under the pointer.
    pub danger: Colour,
    /// Success.
    pub success: Colour,
    /// Warnings.
    pub warning: Colour,
    /// Shadows are this colour ...
    pub shadow: Colour,
    /// ... at this strength under the active window,
    pub shadow_active: u8,
    /// ... under the others,
    pub shadow_inactive: u8,
    /// ... and under menus and panels.
    pub shadow_popup: u8,
    /// The edge line around windows.
    pub edge: Tone,
    /// The desktop background: a gradient through these three colours,
    /// from the top-left corner to the bottom-right, the middle one at
    /// 60% of the way.
    pub wallpaper: [Colour; 3],
}

impl Theme {
    /// The colours for `mode` with `accent`.
    pub const fn new(mode: Mode, accent: Accent) -> Self {
        let colour = accent.colour();
        match mode {
            Mode::Light => Self {
                mode,
                accent_name: accent,
                accent: colour,
                accent_text: colour,
                on_accent: WHITE,
                accent_soft: Tone::new(colour, 130),
                window: 0xFF_FF_FF,
                window_raised: 0xF7_F7_F9,
                // "Sidebar glass", 240 240 245 at 80%, on the window colour.
                chrome: 0xF3_F3_F7,
                control: 0xFF_FF_FF,
                text: 0x1D_1D_21,
                text_2: 0x4E_4E_57,
                text_3: 0x69_69_73,
                separator: Tone::new(BLACK, 80),
                fill: Tone::new(BLACK, 45),
                fill_2: Tone::new(BLACK, 75),
                hover: Tone::new(BLACK, 50),
                press: Tone::new(BLACK, 90),
                bar: Tone::new(0xF6_F6_FA, 740),
                glass: Tone::new(0xF6_F6_FA, 780),
                highlight: Tone::new(WHITE, 700),
                running: Tone::new(BLACK, 340),
                danger: 0xD9_3A_30,
                success: 0x17_82_4A,
                warning: 0xA8_63_00,
                shadow: BLACK,
                shadow_active: 56,
                shadow_inactive: 28,
                shadow_popup: 44,
                edge: Tone::new(BLACK, 160),
                wallpaper: [0x54_70_BE, 0xB9_A6_DD, 0xF4_D3_C4],
            },
            Mode::Dark => Self {
                mode,
                accent_name: accent,
                accent: colour,
                // The accent with 45% white mixed in.
                accent_text: lighten(colour),
                on_accent: WHITE,
                accent_soft: Tone::new(colour, 300),
                window: 0x1F_1F_23,
                window_raised: 0x23_23_27,
                // 40 40 47 at 74% on the window colour.
                chrome: 0x26_26_2C,
                control: 0x3A_3A_41,
                text: 0xF2_F2_F5,
                text_2: 0xBC_BC_C5,
                text_3: 0x96_96_A0,
                separator: Tone::new(WHITE, 80),
                fill: Tone::new(WHITE, 60),
                fill_2: Tone::new(WHITE, 100),
                hover: Tone::new(WHITE, 70),
                press: Tone::new(WHITE, 120),
                bar: Tone::new(0x1A_1A_20, 700),
                glass: Tone::new(0x28_28_2F, 780),
                highlight: Tone::new(WHITE, 110),
                running: Tone::new(WHITE, 500),
                danger: 0xD9_3A_30,
                success: 0x17_82_4A,
                warning: 0xA8_63_00,
                shadow: BLACK,
                shadow_active: 120,
                shadow_inactive: 70,
                shadow_popup: 100,
                edge: Tone::new(BLACK, 800),
                wallpaper: [0x14_14_3A, 0x2B_27_66, 0x4D_35_72],
            },
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(Mode::Light, Accent::Blue)
    }
}

/// `colour` with 45% white mixed in.
const fn lighten(colour: Colour) -> Colour {
    const fn channel(colour: Colour, shift: u32) -> Colour {
        let value = (colour >> shift) & 0xFF;
        ((value * 55 + 255 * 45 + 50) / 100) << shift
    }
    channel(colour, 16) | channel(colour, 8) | channel(colour, 0)
}

/// How easily text in `text` is read on `background`: the contrast ratio
/// of WCAG 2, times 100 (so 450 is the 4.5:1 that body text needs).
pub fn contrast(text: Colour, background: Colour) -> u32 {
    // Relative luminance, in units of 1/1000, from gamma-encoded
    // channels; the gamma curve is approximated by its 2.2 power law
    // through a table of 16 steps, which is accurate to about 1%.
    fn linear(value: u32) -> u32 {
        const TABLE: [u32; 17] = [
            0, 2, 10, 25, 47, 77, 115, 162, 218, 283, 358, 443, 538, 644, 760, 886, 1000,
        ];
        let step = (value / 16) as usize;
        let rest = value % 16;
        TABLE[step] + (TABLE[(step + 1).min(16)] - TABLE[step]) * rest / 16
    }
    let luminance = |colour: Colour| {
        (2126 * linear((colour >> 16) & 0xFF)
            + 7152 * linear((colour >> 8) & 0xFF)
            + 722 * linear(colour & 0xFF))
            / 10_000
    };
    let (a, b) = (luminance(text), luminance(background));
    let (lighter, darker) = (a.max(b), a.min(b));
    (lighter + 50) * 100 / (darker + 50)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_theme() -> impl Iterator<Item = Theme> {
        [Mode::Light, Mode::Dark].into_iter().flat_map(|mode| {
            Accent::ALL
                .into_iter()
                .map(move |accent| Theme::new(mode, accent))
        })
    }

    #[test]
    fn contrast_matches_known_values() {
        // Black on white is 21:1; a colour on itself 1:1.
        assert!(
            (2050..=2150).contains(&contrast(BLACK, WHITE)),
            "{}",
            contrast(BLACK, WHITE)
        );
        assert_eq!(contrast(0x80_80_80, 0x80_80_80), 100);
        assert_eq!(contrast(BLACK, WHITE), contrast(WHITE, BLACK));
        // #767676 on white is the classic "just passes" grey: 4.54:1.
        let grey = contrast(0x76_76_76, WHITE);
        assert!((440..=470).contains(&grey), "{grey}");
    }

    #[test]
    fn text_is_readable_on_every_surface_in_every_theme() {
        for theme in every_theme() {
            let name = (theme.mode, theme.accent_name);
            for surface in [theme.window, theme.window_raised, theme.chrome] {
                for text in [theme.text, theme.text_2, theme.text_3] {
                    assert!(
                        contrast(text, surface) >= 450,
                        "{name:?}: {text:06x} on {surface:06x}"
                    );
                }
                assert!(
                    contrast(theme.accent_text, surface) >= 430,
                    "{name:?}: accent text on {surface:06x}: {}",
                    contrast(theme.accent_text, surface)
                );
                // Text stays readable on a hovered or selected row.
                let hovered = theme.fill_2.over(surface);
                assert!(contrast(theme.text, hovered) >= 450, "{name:?}");
                let selected = theme.accent_soft.over(surface);
                assert!(contrast(theme.text, selected) >= 450, "{name:?}");
            }
            assert!(
                contrast(theme.on_accent, theme.accent) >= 450,
                "{name:?}: on accent"
            );
            assert!(contrast(WHITE, theme.danger) >= 400, "{name:?}: on danger");
        }
    }

    #[test]
    fn the_modes_differ_as_they_should() {
        let light = Theme::new(Mode::Light, Accent::Teal);
        let dark = Theme::new(Mode::Dark, Accent::Teal);
        assert_eq!(light.accent, dark.accent);
        assert_eq!(light.accent_text, light.accent);
        // Lightened in dark mode: every channel at least as bright.
        for shift in [16, 8, 0] {
            assert!((dark.accent_text >> shift) & 0xFF >= (dark.accent >> shift) & 0xFF);
        }
        assert_eq!(lighten(0x00_00_00), 0x73_73_73);
        assert_eq!(lighten(0xFF_FF_FF), 0xFF_FF_FF);
        assert!(contrast(light.text, WHITE) > 1000 && contrast(dark.text, BLACK) > 1000);
        assert_eq!(Theme::default(), Theme::new(Mode::Light, Accent::Blue));
        // The six accents are six different colours.
        for (index, a) in Accent::ALL.iter().enumerate() {
            for b in &Accent::ALL[index + 1..] {
                assert_ne!(a.colour(), b.colour());
                assert_ne!(a.name(), b.name());
            }
        }
    }

    #[test]
    fn tones() {
        assert_eq!(Tone::new(BLACK, 1000).alpha, 255);
        assert_eq!(Tone::new(BLACK, 0).alpha, 0);
        assert_eq!(Tone::new(BLACK, 80).alpha, 20);
        assert_eq!(Tone::new(BLACK, 500).over(WHITE), 0x7F_7F_7F);
        assert_eq!(Tone::new(WHITE, 1000).over(BLACK), WHITE);
    }
}
