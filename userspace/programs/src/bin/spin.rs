// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Computes forever without making system calls. Exists to show that a
//! user program cannot monopolise the CPU and can be killed.

#![no_std]
#![no_main]

omit_rt::entry!(main);

fn main() -> i32 {
    let mut value: u64 = 1;
    loop {
        value = core::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}
