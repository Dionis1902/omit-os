// SPDX-License-Identifier: GPL-2.0-only
//! `scribble` — a small drawing program with its own window.
//!
//! Hold the left mouse button and move to draw. Keys: `c` clears, `1` to
//! `4` pick a colour, `q` quits. The title bar counts the strokes.
//!
//! It exists to show (and test) that a program other than the desktop
//! can have a window: the pixels are memory shared with the desktop,
//! input arrives over a channel.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;

use omit_rt::eprintln;
use omit_rt::window::{Event, Window};

omit_rt::entry!(main);

const WIDTH: usize = 360;
const HEIGHT: usize = 240;
const PAPER: u32 = 0xFF_FF_F0;
const INKS: [u32; 4] = [0x20_20_20, 0xC0_20_20, 0x20_80_30, 0x20_40_C0];
/// Height of the strip at the top that shows the current colour.
const SWATCH: usize = 8;

/// Draws a 3x3 dot.
fn dot(window: &mut Window, x: i32, y: i32, colour: u32) {
    let (width, height) = (window.width() as i32, window.height() as i32);
    let pixels = window.pixels();
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (px, py) = (x + dx, y + dy);
            if (0..width).contains(&px) && (SWATCH as i32..height).contains(&py) {
                pixels[(py * width + px) as usize] = colour;
            }
        }
    }
}

/// Draws a line of dots from one point to another.
fn line(window: &mut Window, from: (i32, i32), to: (i32, i32), colour: u32) {
    let steps = (to.0 - from.0).abs().max((to.1 - from.1).abs()).max(1);
    for step in 0..=steps {
        dot(
            window,
            from.0 + (to.0 - from.0) * step / steps,
            from.1 + (to.1 - from.1) * step / steps,
            colour,
        );
    }
}

fn clear(window: &mut Window, ink: u32) {
    let width = window.width();
    let pixels = window.pixels();
    pixels[..SWATCH * width].fill(ink);
    pixels[SWATCH * width..].fill(PAPER);
}

fn main() -> i32 {
    let mut window = match Window::open("Scribble", WIDTH, HEIGHT) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("scribble: cannot open a window: {error}");
            return 1;
        }
    };
    let mut ink = INKS[0];
    let mut strokes = 0u32;
    // Where the stroke being drawn last was.
    let mut last: Option<(i32, i32)> = None;
    clear(&mut window, ink);
    window.present();

    loop {
        match window.event(None) {
            Some(Event::Pointer { x, y, buttons }) if buttons & 1 != 0 => {
                if last.is_none() {
                    strokes += 1;
                    window.set_title(&format!("Scribble - {strokes} stroke(s)"));
                }
                line(&mut window, last.unwrap_or((x, y)), (x, y), ink);
                last = Some((x, y));
                window.present();
            }
            Some(Event::Pointer { .. }) => last = None,
            Some(Event::Key(code)) => match u8::try_from(code).map(char::from) {
                Ok('q') => return 0,
                Ok('c') => {
                    strokes = 0;
                    clear(&mut window, ink);
                    window.set_title("Scribble");
                    window.present();
                }
                Ok(digit @ '1'..='4') => {
                    ink = INKS[digit as usize - '1' as usize];
                    let width = window.width();
                    window.pixels()[..SWATCH * width].fill(ink);
                    window.present_area(0, 0, width, SWATCH);
                }
                _ => {}
            },
            Some(Event::Closed) => return 0,
            None => {}
        }
    }
}
