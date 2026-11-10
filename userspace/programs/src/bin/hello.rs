// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Prints a greeting from user mode.

#![no_std]
#![no_main]

use omit_rt::println;

omit_rt::entry!(main);

fn main() -> i32 {
    println!(
        "Hello from user mode! I am process {} and the system has been up for {} ms.",
        omit_rt::getpid(),
        omit_rt::uptime_ms()
    );
    0
}
