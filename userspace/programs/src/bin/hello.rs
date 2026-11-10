// SPDX-License-Identifier: GPL-2.0-only
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
