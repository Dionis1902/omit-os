// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Prints a line every 100 ms, five times, then exits with status 7.

#![no_std]
#![no_main]

use omit_rt::println;

omit_rt::entry!(main);

fn main() -> i32 {
    for tick in 1..=5 {
        println!("sleeper: tick {tick}");
        omit_rt::sleep_ms(100);
    }
    7
}
