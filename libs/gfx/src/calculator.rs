// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Calculator window: the four operations, percent and sign, by
//! mouse or keyboard.
//!
//! It works the way pocket calculators do: each operator acts on what
//! is shown as soon as the next one (or `=`) is pressed, so `2 + 3 × 4`
//! is 20; `=` again repeats the last operation. Numbers are binary
//! floating point shown to ten significant digits, which hides the
//! usual surprises (0.1 + 0.2 shows 0.3).
//!
//! The design system's calculator also has scientific and conversion
//! modes; this is its basic one.

use alloc::format;
use alloc::string::String;

use crate::desktop::{App, Context};
use crate::font::{MEDIUM_13, REGULAR_12, SEMIBOLD_20, SEMIBOLD_32};
use crate::geometry::{Point, Rect};
use crate::surface::Canvas;
use crate::theme::Theme;

/// The size of the window's content area; it is not resizable.
pub const SIZE: (i32, i32) = (270, 380);

/// An operation on two numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `+`
    Add,
    /// `−`
    Subtract,
    /// `×`
    Multiply,
    /// `÷`
    Divide,
}

impl Operator {
    fn sign(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "−",
            Self::Multiply => "×",
            Self::Divide => "÷",
        }
    }

    fn apply(self, left: f64, right: f64) -> Option<f64> {
        let result = match self {
            Self::Add => left + right,
            Self::Subtract => left - right,
            Self::Multiply => left * right,
            Self::Divide if right == 0.0 => return None,
            Self::Divide => left / right,
        };
        result.is_finite().then_some(result)
    }
}

/// A key of the calculator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A digit, 0 to 9.
    Digit(u8),
    /// The decimal point.
    Point,
    /// An operator.
    Operator(Operator),
    /// `=`
    Equals,
    /// `C`: start again.
    Clear,
    /// `±`: change the sign of what is shown.
    Sign,
    /// `%`: a hundredth of what is shown — of the first number, if an
    /// operation is under way (`200 + 10 %` is 220).
    Percent,
    /// Take back the digit typed last.
    Backspace,
}

/// The keys as they are laid out, row by row; `None` is where the wide
/// zero key continues.
const LAYOUT: [[Option<Key>; 4]; 5] = [
    [
        Some(Key::Clear),
        Some(Key::Sign),
        Some(Key::Percent),
        Some(Key::Operator(Operator::Divide)),
    ],
    [
        Some(Key::Digit(7)),
        Some(Key::Digit(8)),
        Some(Key::Digit(9)),
        Some(Key::Operator(Operator::Multiply)),
    ],
    [
        Some(Key::Digit(4)),
        Some(Key::Digit(5)),
        Some(Key::Digit(6)),
        Some(Key::Operator(Operator::Subtract)),
    ],
    [
        Some(Key::Digit(1)),
        Some(Key::Digit(2)),
        Some(Key::Digit(3)),
        Some(Key::Operator(Operator::Add)),
    ],
    [
        Some(Key::Digit(0)),
        None,
        Some(Key::Point),
        Some(Key::Equals),
    ],
];

const MARGIN: i32 = 12;
const GAP: i32 = 6;
const DISPLAY: i32 = 104;
const KEY: (i32, i32) = (57, 48);
/// Most digits that can be typed into one number.
const MAX_DIGITS: usize = 12;

/// Where in the content area a key is, if it is on the keypad
/// (`Backspace` is a key of the keyboard only).
pub fn key_frame(key: Key) -> Option<Rect> {
    for (row, keys) in LAYOUT.iter().enumerate() {
        for (column, slot) in keys.iter().enumerate() {
            if *slot == Some(key) {
                let wide = keys.get(column + 1) == Some(&None);
                return Some(Rect::new(
                    MARGIN + column as i32 * (KEY.0 + GAP),
                    DISPLAY + row as i32 * (KEY.1 + GAP),
                    if wide { 2 * KEY.0 + GAP } else { KEY.0 },
                    KEY.1,
                ));
            }
        }
    }
    None
}

/// A number as the calculator shows it: up to ten significant digits,
/// no trailing zeros, and powers of ten for what is too large or too
/// small to write out.
pub fn show(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let size = value.abs();
    if !(1e-7..1e12).contains(&size) {
        // Seven digits and the power of ten.
        let text = format!("{value:.6e}");
        let (digits, power) = text.split_once('e').unwrap_or((&text, "0"));
        let digits = digits.trim_end_matches('0').trim_end_matches('.');
        return format!("{digits}e{power}");
    }
    // Digits before the point decide how many are left for after it.
    let mut whole_digits = 1;
    let mut limit = 10.0;
    while size >= limit {
        whole_digits += 1;
        limit *= 10.0;
    }
    // (Below one, the zero before the point does not count.)
    let decimals = if size < 1.0 {
        10
    } else {
        10usize.saturating_sub(whole_digits)
    };
    let text = format!("{value:.decimals$}");
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        &text
    };
    // Rounding can leave "-0".
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

/// The calculator itself, without its window.
#[derive(Debug, Clone, PartialEq)]
pub struct Calculator {
    /// What is being typed, as typed; empty when a result is shown.
    entry: String,
    /// What is shown when nothing is being typed.
    value: f64,
    /// The first number of the operation under way, and the operation.
    pending: Option<(f64, Operator)>,
    /// The last completed operation's operator and second number, for
    /// repeating with `=`.
    last: Option<(Operator, f64)>,
    /// What led to the result shown, for the line above it.
    history: String,
    /// Set by a division by zero or a result too large to hold.
    error: bool,
}

impl Default for Calculator {
    fn default() -> Self {
        Self::new()
    }
}

impl Calculator {
    /// A calculator showing 0.
    pub fn new() -> Self {
        Self {
            entry: String::new(),
            value: 0.0,
            pending: None,
            last: None,
            history: String::new(),
            error: false,
        }
    }

    /// What the display shows.
    pub fn display(&self) -> String {
        if self.error {
            "Cannot be done".into()
        } else if self.entry.is_empty() {
            show(self.value)
        } else {
            self.entry.clone()
        }
    }

    /// The line above the display: the operation under way or the one
    /// that gave the result.
    pub fn history(&self) -> &str {
        &self.history
    }

    /// The operator waiting for its second number, if nothing of that
    /// number has been typed yet.
    pub fn waiting(&self) -> Option<Operator> {
        self.pending
            .filter(|_| self.entry.is_empty())
            .map(|(_, operator)| operator)
    }

    /// The number shown, as a number.
    fn shown(&self) -> f64 {
        if self.entry.is_empty() {
            self.value
        } else {
            self.entry.parse().unwrap_or(0.0)
        }
    }

    fn fail(&mut self) {
        *self = Self::new();
        self.error = true;
    }

    /// Carries out the operation under way with the number shown.
    fn complete(&mut self) -> bool {
        let Some((left, operator)) = self.pending.take() else {
            return true;
        };
        let right = self.shown();
        match operator.apply(left, right) {
            Some(result) => {
                self.history = format!("{} {} {} =", show(left), operator.sign(), show(right));
                self.value = result;
                self.entry.clear();
                self.last = Some((operator, right));
                true
            }
            None => {
                self.fail();
                false
            }
        }
    }

    /// Presses a key.
    pub fn press(&mut self, key: Key) {
        if self.error {
            // Anything clears the message; a digit also starts a number.
            *self = Self::new();
            if !matches!(key, Key::Digit(_) | Key::Point) {
                return;
            }
        }
        match key {
            Key::Digit(digit) => {
                let digits = self.entry.bytes().filter(u8::is_ascii_digit).count();
                if digits < MAX_DIGITS {
                    if self.entry == "0" {
                        self.entry.clear();
                    } else if self.entry == "-0" {
                        self.entry.truncate(1);
                    }
                    self.entry.push(char::from(b'0' + digit.min(9)));
                }
            }
            Key::Point => {
                if self.entry.is_empty() {
                    self.entry.push('0');
                }
                if !self.entry.contains('.') {
                    self.entry.push('.');
                }
            }
            Key::Backspace => {
                self.entry.pop();
                if self.entry == "-" {
                    self.entry.clear();
                }
                if self.entry.is_empty() && self.pending.is_none() {
                    self.value = 0.0;
                }
            }
            Key::Sign => {
                if self.entry.is_empty() {
                    self.value = -self.value;
                } else if let Some(rest) = self.entry.strip_prefix('-') {
                    self.entry = rest.into();
                } else {
                    self.entry.insert(0, '-');
                }
            }
            Key::Percent => {
                let shown = self.shown();
                self.value = match self.pending {
                    Some((left, _)) => left * shown / 100.0,
                    None => shown / 100.0,
                };
                self.entry.clear();
                if self.pending.is_some() {
                    // The result is the second number: show it as typed.
                    self.entry = show(self.value);
                }
            }
            Key::Operator(operator) => {
                // Pressed straight after another operator: a change of mind.
                if self.waiting().is_none() && !self.complete() {
                    return;
                }
                let left = self.shown();
                self.value = left;
                self.entry.clear();
                self.pending = Some((left, operator));
                self.history = format!("{} {}", show(left), operator.sign());
                self.last = None;
            }
            Key::Equals => {
                if self.pending.is_some() {
                    self.complete();
                } else if let Some((operator, right)) = self.last {
                    // Again, with the result as the first number.
                    let left = self.shown();
                    match operator.apply(left, right) {
                        Some(result) => {
                            self.history =
                                format!("{} {} {} =", show(left), operator.sign(), show(right));
                            self.value = result;
                            self.entry.clear();
                        }
                        None => self.fail(),
                    }
                } else {
                    self.value = self.shown();
                    self.entry.clear();
                }
            }
            Key::Clear => *self = Self::new(),
        }
    }
}

/// The key a keyboard key stands for.
fn from_keyboard(code: i32) -> Option<Key> {
    Some(match u8::try_from(code).ok()? {
        digit @ b'0'..=b'9' => Key::Digit(digit - b'0'),
        b'.' | b',' => Key::Point,
        b'+' => Key::Operator(Operator::Add),
        b'-' => Key::Operator(Operator::Subtract),
        b'*' | b'x' => Key::Operator(Operator::Multiply),
        b'/' => Key::Operator(Operator::Divide),
        b'=' | b'\n' | b'\r' => Key::Equals,
        b'%' => Key::Percent,
        b'c' | b'C' | 27 => Key::Clear,
        8 | 127 => Key::Backspace,
        _ => return None,
    })
}

/// The window's contents.
#[derive(Default)]
pub struct Window {
    calculator: Calculator,
}

impl Window {
    /// The window, showing 0.
    pub fn new() -> Self {
        Self::default()
    }
}

impl App for Window {
    fn title(&self) -> &str {
        "Calculator"
    }

    fn icon(&self) -> &str {
        "calculator"
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, width: i32, _height: i32, theme: &Theme) {
        // The display: the operation above, the number below, both at
        // the right.
        let right = width - MARGIN - 6;
        let history = self.calculator.history();
        canvas.write(
            right - REGULAR_12.width(history),
            22,
            history,
            REGULAR_12,
            theme.text_3,
        );
        let shown = self.calculator.display();
        // Long numbers are set smaller rather than cut off.
        let font = if SEMIBOLD_32.width(&shown) <= width - 2 * MARGIN - 12 {
            SEMIBOLD_32
        } else {
            SEMIBOLD_20
        };
        canvas.write(
            right - font.width(&shown),
            48 + (SEMIBOLD_32.height() - font.height()) / 2,
            &shown,
            font,
            theme.text,
        );

        let waiting = self.calculator.waiting();
        for key in LAYOUT.iter().flatten().flatten() {
            let Some(area) = key_frame(*key) else {
                continue;
            };
            let label = match key {
                Key::Digit(digit) => format!("{digit}"),
                Key::Point => ".".into(),
                Key::Operator(operator) => operator.sign().into(),
                Key::Equals => "=".into(),
                Key::Clear => "C".into(),
                Key::Sign => "±".into(),
                Key::Percent => "%".into(),
                Key::Backspace => String::new(),
            };
            // Digits are quiet keys; operators are tinted with the
            // accent; `=` and the operator in force are the accent.
            let ink = match key {
                Key::Equals => {
                    canvas.round_rect(area, 10, theme.accent, 255);
                    theme.on_accent
                }
                Key::Operator(operator) if waiting == Some(*operator) => {
                    canvas.round_rect(area, 10, theme.accent, 255);
                    theme.on_accent
                }
                Key::Operator(_) => {
                    canvas.round_rect(area, 10, theme.accent_soft.colour, theme.accent_soft.alpha);
                    theme.accent_text
                }
                Key::Digit(_) | Key::Point => {
                    canvas.round_rect(area, 10, theme.fill_2.colour, theme.fill_2.alpha);
                    theme.text
                }
                _ => {
                    canvas.round_rect(area, 10, theme.fill.colour, theme.fill.alpha);
                    theme.text_2
                }
            };
            let font = if matches!(key, Key::Clear | Key::Percent) {
                MEDIUM_13
            } else {
                SEMIBOLD_20
            };
            canvas.write(
                area.x + (area.w - font.width(&label)) / 2,
                area.y + (area.h - font.height()) / 2,
                &label,
                font,
                ink,
            );
        }
    }

    fn click(&mut self, x: i32, y: i32, _: &mut Context) {
        let at = Point::new(x, y);
        let key = LAYOUT
            .iter()
            .flatten()
            .flatten()
            .find(|key| key_frame(**key).is_some_and(|area| area.contains(at)));
        if let Some(key) = key {
            self.calculator.press(*key);
        }
    }

    fn key(&mut self, code: i32, _: &mut Context) {
        if let Some(key) = from_keyboard(code) {
            self.calculator.press(key);
        }
    }

    fn resizable(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Surface;

    /// Types `keys` — digits, `. + - * / = % c`, `n` for the sign key,
    /// `<` for backspace — and returns what is shown.
    fn run(keys: &str) -> String {
        let mut calculator = Calculator::new();
        for key in keys.chars() {
            calculator.press(match key {
                'n' => Key::Sign,
                '<' => Key::Backspace,
                other => from_keyboard(other as i32).unwrap_or_else(|| panic!("{other}")),
            });
        }
        calculator.display()
    }

    #[test]
    fn the_four_operations() {
        assert_eq!(run(""), "0");
        assert_eq!(run("12"), "12");
        assert_eq!(run("1+2="), "3");
        assert_eq!(run("10-25="), "-15");
        assert_eq!(run("6*7="), "42");
        assert_eq!(run("1/8="), "0.125");
        assert_eq!(run("2.5*4="), "10");
        assert_eq!(run("0.1+0.2="), "0.3");
        assert_eq!(run("1/3="), "0.3333333333");
        assert_eq!(run("2/3="), "0.6666666667");
        assert_eq!(run("123456789*1000="), "123456789000");
    }

    #[test]
    fn operations_are_carried_out_as_they_come() {
        assert_eq!(run("2+3*4="), "20");
        assert_eq!(run("2+3*"), "5", "the sum so far is shown");
        assert_eq!(run("10-2-3-1="), "4");
        // `=` again repeats the last operation.
        assert_eq!(run("2*3=="), "18");
        assert_eq!(run("100-1===="), "96");
        // A result is the first number of what follows.
        assert_eq!(run("2+3=*10="), "50");
        // A number typed after a result starts afresh.
        assert_eq!(run("2+3=7"), "7");
        assert_eq!(run("2+3=7*2="), "14");
        // A change of mind about the operator.
        assert_eq!(run("8+-*2="), "16");
        // `=` with nothing to do.
        assert_eq!(run("5="), "5");
        assert_eq!(run("="), "0");
    }

    #[test]
    fn typing_numbers() {
        assert_eq!(run("007"), "7");
        assert_eq!(run("0.50"), "0.50", "what is typed is shown as typed");
        assert_eq!(run("."), "0.");
        assert_eq!(run("1.2.3"), "1.23", "one decimal point");
        assert_eq!(run("12<"), "1");
        assert_eq!(run("12<<<"), "0");
        assert_eq!(run("1.<"), "1");
        // At most twelve digits.
        assert_eq!(run("12345678901234567"), "123456789012");
        // The sign key.
        assert_eq!(run("5n"), "-5");
        assert_eq!(run("5nn"), "5");
        assert_eq!(run("5n+2="), "-3");
        assert_eq!(run("3+2=n"), "-5");
        assert_eq!(run("n5"), "5", "the sign of nothing is nothing");
        assert_eq!(run("5n<"), "0");
        // Starting over.
        assert_eq!(run("5+3c"), "0");
        assert_eq!(run("5+3c2="), "2");
    }

    #[test]
    fn percent() {
        assert_eq!(run("50%"), "0.5");
        assert_eq!(run("200+10%="), "220");
        assert_eq!(run("200-10%="), "180");
        assert_eq!(run("50*10%="), "250", "ten percent of fifty is five");
        assert_eq!(run("200+10%"), "20");
    }

    #[test]
    fn what_cannot_be_done_says_so_and_then_starts_again() {
        assert_eq!(run("5/0="), "Cannot be done");
        assert_eq!(run("5/0+"), "Cannot be done");
        assert_eq!(run("5/0=7"), "7");
        assert_eq!(run("5/0=+"), "0");
        assert_eq!(run("5/0==3*2="), "6");
        // Too large to hold.
        let mut calculator = Calculator::new();
        for _ in 0..12 {
            calculator.press(Key::Digit(9));
        }
        calculator.press(Key::Operator(Operator::Multiply));
        let mut presses = 0;
        while calculator.display() != "Cannot be done" {
            calculator.press(Key::Equals);
            presses += 1;
            assert!(presses < 40, "the number never became too large");
        }
        // The key after the message starts again from nothing.
        calculator.press(Key::Equals);
        assert_eq!(calculator.display(), "0");
    }

    #[test]
    fn the_line_above_shows_the_operation() {
        let mut calculator = Calculator::new();
        assert_eq!(calculator.history(), "");
        for key in [
            Key::Digit(1),
            Key::Digit(2),
            Key::Operator(Operator::Divide),
        ] {
            calculator.press(key);
        }
        assert_eq!(calculator.history(), "12 ÷");
        assert_eq!(calculator.waiting(), Some(Operator::Divide));
        calculator.press(Key::Digit(5));
        assert_eq!(
            calculator.waiting(),
            None,
            "its second number has been started"
        );
        calculator.press(Key::Equals);
        assert_eq!(calculator.history(), "12 ÷ 5 =");
        assert_eq!(calculator.display(), "2.4");
        calculator.press(Key::Equals);
        assert_eq!(calculator.history(), "2.4 ÷ 5 =");
        calculator.press(Key::Clear);
        assert_eq!(calculator.history(), "");
    }

    #[test]
    fn numbers_are_shown_to_ten_digits() {
        for (value, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (1234.5678, "1234.5678"),
            (0.000_001, "0.000001"),
            (123_456_789_012.0, "123456789012"),
            (99_999_999_999.5, "100000000000"),
            (1e12, "1e12"),
            (1.5e20, "1.5e20"),
            (-2.5e-9, "-2.5e-9"),
            (1e-7, "0.0000001"),
            (9.999_999_999_9e-8, "1e-7"),
            (1.0 / 3.0, "0.3333333333"),
            (-0.000_000_000_01 + 0.000_000_000_01, "0"),
        ] {
            assert_eq!(show(value), text, "{value:e}");
        }
    }

    #[test]
    fn the_keypad_is_a_grid_and_every_key_can_be_clicked() {
        let content = Rect::new(0, 0, SIZE.0, SIZE.1);
        let mut frames = alloc::vec::Vec::new();
        for key in LAYOUT.iter().flatten().flatten() {
            let area = key_frame(*key).unwrap();
            assert_eq!(
                area.intersect(&content),
                area,
                "{key:?} is inside the window"
            );
            assert!(area.y >= DISPLAY);
            for (other, other_area) in &frames {
                assert!(
                    area.intersect(other_area).is_empty(),
                    "{key:?} and {other:?}"
                );
            }
            frames.push((*key, area));
        }
        assert_eq!(frames.len(), 19);
        assert_eq!(key_frame(Key::Backspace), None);
        // The zero key is two keys wide.
        assert_eq!(key_frame(Key::Digit(0)).unwrap().w, 2 * KEY.0 + GAP);
        // The right edge of the grid is as far from the window's edge
        // as the left.
        let last = key_frame(Key::Equals).unwrap();
        assert_eq!(SIZE.0 - last.right(), MARGIN);
        assert_eq!(SIZE.1 - last.bottom(), MARGIN);

        // Clicking 7, ×, 6, = through the window.
        let mut window = Window::new();
        let mut context = Context::default();
        for key in [
            Key::Digit(7),
            Key::Operator(Operator::Multiply),
            Key::Digit(6),
            Key::Equals,
        ] {
            let area = key_frame(key).unwrap();
            window.click(area.x + 5, area.y + 5, &mut context);
        }
        assert_eq!(window.calculator.display(), "42");
        // Between keys and in the display: nothing.
        window.click(MARGIN + KEY.0 + 2, DISPLAY + 5, &mut context);
        window.click(100, 40, &mut context);
        assert_eq!(window.calculator.display(), "42");
        // And by keyboard.
        for code in [27, b'9' as i32, b'/' as i32, b'4' as i32, 13] {
            window.key(code, &mut context);
        }
        assert_eq!(window.calculator.display(), "2.25");
        window.key(0x101, &mut context);
        assert_eq!(window.calculator.display(), "2.25");
    }

    #[test]
    fn the_window_draws_the_number_and_marks_the_operator_in_force() {
        let theme = Theme::default();
        let mut window = Window::new();
        let mut context = Context::default();
        let draw = |window: &mut Window| {
            let mut surface = Surface::new(SIZE.0 as usize, SIZE.1 as usize, theme.window);
            window.draw(&mut surface.canvas(), SIZE.0, SIZE.1, &theme);
            surface
        };
        let empty = draw(&mut window);
        let plus = key_frame(Key::Operator(Operator::Add)).unwrap();
        let minus = key_frame(Key::Operator(Operator::Subtract)).unwrap();
        let corner = |area: Rect| (area.x + 12, area.y + 6);
        let (px, py) = corner(plus);
        let (mx, my) = corner(minus);
        assert_eq!(
            empty.pixel(px, py),
            Some(theme.accent_soft.over(theme.window))
        );
        // After "8 +", the plus key is filled with the accent; once a
        // digit follows it is an ordinary operator key again.
        window.key(i32::from(b'8'), &mut context);
        window.key(i32::from(b'+'), &mut context);
        let waiting = draw(&mut window);
        assert_eq!(waiting.pixel(px, py), Some(theme.accent));
        assert_eq!(
            waiting.pixel(mx, my),
            Some(theme.accent_soft.over(theme.window))
        );
        window.key(i32::from(b'1'), &mut context);
        assert_eq!(
            draw(&mut window).pixel(px, py),
            Some(theme.accent_soft.over(theme.window))
        );
        // The display changes with the number, at the right-hand side.
        let display = Rect::new(0, 0, SIZE.0, DISPLAY);
        let differs = |a: &Surface, b: &Surface, area: Rect| {
            (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| a.pixel(x, y) != b.pixel(x, y))
                .count()
        };
        assert!(differs(&empty, &waiting, display) > 40);
        assert_eq!(differs(&empty, &waiting, Rect::new(0, 0, 60, DISPLAY)), 0);
        // A very long number still fits: it is set smaller.
        let mut long = Window::new();
        for _ in 0..12 {
            long.key(i32::from(b'8'), &mut context);
        }
        long.key(i32::from(b'.'), &mut context);
        let surface = draw(&mut long);
        assert_eq!(differs(&surface, &empty, Rect::new(0, 40, MARGIN, 60)), 0);
    }
}
