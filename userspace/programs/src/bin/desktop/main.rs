// SPDX-License-Identifier: GPL-2.0-only
//! `desktop` — a graphical desktop.
//!
//! Takes over the screen, keyboard and mouse from the text console and
//! shows movable windows: a file browser, a text viewer, a window for
//! running programs, and an information window. The task bar's **Exit**
//! button, or F12, gives the screen back to the console.
//!
//! The window manager and all drawing are in the `omit-gfx` library; this
//! program provides the windows' contents ([`apps`]) and the loop that
//! feeds input events to the desktop and copies what changed to the
//! screen. Everything runs in this one process: there is no way yet for
//! other programs to open windows.

#![no_std]
#![no_main]

extern crate alloc;

mod apps;

use alloc::boxed::Box;
use alloc::format;

use omit_gfx::desktop::Desktop;
use omit_gfx::surface::Surface;
use omit_rt::abi::{Errno, InputEvent, event, key};
use omit_rt::{eprintln, println};

omit_rt::entry!(main);

/// How long to wait for input before updating clocks and polling running
/// programs anyway.
const IDLE_MS: u64 = 200;

fn main() -> i32 {
    let info = match omit_rt::display_info() {
        Ok(info) => info,
        Err(_) => {
            eprintln!("desktop: this machine has no screen");
            return 1;
        }
    };
    match omit_rt::display_acquire() {
        Ok(()) => {}
        Err(Errno::Busy) => {
            eprintln!("desktop: the screen is in use by another program");
            return 1;
        }
        Err(error) => {
            eprintln!("desktop: cannot take over the screen: {error}");
            return 1;
        }
    }
    let (width, height) = (info.width as usize, info.height as usize);
    let mut surface = Surface::new(width, height, 0);
    let mut desktop = Desktop::new(width, height);

    desktop.add_launcher("Files", |context| {
        context.open(Box::new(apps::Files::new("/")), 440, 330);
    });
    desktop.add_launcher("Run", |context| {
        context.open(Box::new(apps::Run::new()), 620, 360);
    });
    desktop.add_launcher("About", move |context| {
        context.open(Box::new(apps::About::new(width, height)), 470, 200);
    });
    desktop.add_launcher("Exit", |context| context.quit());
    desktop.open(Box::new(apps::About::new(width, height)), 470, 200);

    let mut events = [InputEvent::default(); 32];
    let mut failure = None;
    let mut latency = Latency::default();
    // When the oldest input not yet visible on the screen arrived.
    let mut pending_since: Option<u64> = None;
    while !desktop.quit_requested() {
        let seconds = omit_rt::uptime_ms() / 1000;
        desktop.set_status(&format!(
            "up {}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        ));
        let dirty = desktop.render(&mut surface);
        if !dirty.is_empty() {
            // The desktop only reports areas inside the screen.
            let shown = omit_rt::display_blit(
                surface.pixels(),
                width,
                dirty.x as usize,
                dirty.y as usize,
                dirty.w as usize,
                dirty.h as usize,
            );
            if let Err(error) = shown {
                failure = Some(error);
                break;
            }
            // What the input led to is on the screen now.
            if let Some(since) = pending_since.take() {
                latency.record(omit_rt::clock_us().saturating_sub(since));
            }
        }
        pending_since = None;

        let count = match omit_rt::read_events(&mut events, IDLE_MS) {
            Ok(count) => count,
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        for input in &events[..count] {
            pending_since = Some(pending_since.map_or(input.time_us, |t| t.min(input.time_us)));
            match input.kind {
                event::KEY if input.a == key::F1 + 11 => {
                    // F12 always leaves, whatever the windows do.
                    return finish(None, &latency);
                }
                event::KEY => desktop.key(input.a),
                event::MOUSE => desktop.mouse(input.a, input.b, input.c),
                _ => {}
            }
        }
        desktop.tick();
    }
    finish(failure, &latency)
}

/// Statistics of the time from an input event reaching the kernel to the
/// screen showing its effect.
#[derive(Default)]
struct Latency {
    updates: u64,
    total_us: u64,
    max_us: u64,
}

impl Latency {
    fn record(&mut self, micros: u64) {
        self.updates += 1;
        self.total_us += micros;
        self.max_us = self.max_us.max(micros);
    }
}

fn finish(failure: Option<Errno>, latency: &Latency) -> i32 {
    let _ = omit_rt::display_release();
    match failure {
        None => {
            println!("desktop: closed");
            if let Some(average) = latency.total_us.checked_div(latency.updates) {
                println!(
                    "desktop: input-to-screen latency over {} updates: average {}.{} ms, \
                     longest {}.{} ms",
                    latency.updates,
                    average / 1000,
                    (average % 1000) / 100,
                    latency.max_us / 1000,
                    (latency.max_us % 1000) / 100
                );
            }
            0
        }
        Some(error) => {
            eprintln!("desktop: display error: {error}");
            1
        }
    }
}
