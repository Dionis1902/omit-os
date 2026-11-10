// SPDX-License-Identifier: GPL-2.0-only
//! `clock` — a window that shows how long the system has been running.
//!
//! A second small program with a window of its own, drawn with the same
//! graphics library the desktop uses. `q` quits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;

use omit_gfx::geometry::Rect;
use omit_gfx::surface::{Surface, TEXT_HEIGHT, text_width};
use omit_rt::eprintln;
use omit_rt::window::{Event, Window};

omit_rt::entry!(main);

const WIDTH: usize = 200;
const HEIGHT: usize = 64;
const BACKGROUND: u32 = 0x10_18_30;
const TEXT: u32 = 0xE8_F0_FF;

fn main() -> i32 {
    let mut window = match Window::open("Clock", WIDTH, HEIGHT) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("clock: cannot open a window: {error}");
            return 1;
        }
    };
    // Drawn here, then copied into the window.
    let mut image = Surface::new(WIDTH, HEIGHT, BACKGROUND);
    let mut shown = u64::MAX;
    loop {
        let seconds = omit_rt::uptime_ms() / 1000;
        if seconds != shown {
            shown = seconds;
            let text = format!(
                "up {}:{:02}:{:02}",
                seconds / 3600,
                (seconds / 60) % 60,
                seconds % 60
            );
            let mut canvas = image.canvas();
            canvas.fill(Rect::new(0, 0, WIDTH as i32, HEIGHT as i32), BACKGROUND);
            canvas.text(
                (WIDTH as i32 - text_width(&text)) / 2,
                (HEIGHT as i32 - TEXT_HEIGHT) / 2,
                &text,
                TEXT,
            );
            window.pixels().copy_from_slice(image.pixels());
            window.present();
        }
        // Until the next second starts, or something happens.
        let wait = 1000 - omit_rt::uptime_ms() % 1000;
        match window.event(Some(wait)) {
            Some(Event::Key(code)) if code == i32::from(b'q') => return 0,
            Some(Event::Closed) => return 0,
            _ => {}
        }
    }
}
