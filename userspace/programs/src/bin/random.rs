// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! `random [COUNT]` — prints random bytes from the system's generator,
//! in hexadecimal (16 of them unless told otherwise, at most 4096).
//!
//! `random --check` asks for two lots instead and checks what can be
//! checked from outside: that they are not blank and not the same. It
//! also says where the system's randomness comes from.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;

use omit_rt::{eprintln, print, println};

omit_rt::entry!(main);

fn source(hardware: bool) -> &'static str {
    if hardware {
        "the processor's random number generator"
    } else {
        "timing only (this processor offers no random numbers): may be guessable"
    }
}

fn check() -> i32 {
    let (mut first, mut second) = ([0u8; 64], [0u8; 64]);
    let hardware = omit_rt::system::random(&mut first);
    omit_rt::system::random(&mut second);
    // 64 zero bytes, or the same 64 bytes twice, do not happen by
    // chance.
    if first == [0; 64] || second == [0; 64] {
        eprintln!("random: the generator gave nothing but zeros");
        return 1;
    }
    if first == second {
        eprintln!("random: the generator gave the same bytes twice");
        return 1;
    }
    // Neither does a lot with fewer than 150 or more than 362 of its
    // 512 bits set (ten standard deviations from the middle).
    for lot in [first, second] {
        let ones: u32 = lot.iter().map(|byte| byte.count_ones()).sum();
        if !(150..=362).contains(&ones) {
            eprintln!("random: {ones} of 512 bits are set");
            return 1;
        }
    }
    println!("random: ok, from {}", source(hardware));
    0
}

fn main() -> i32 {
    let count = match omit_rt::args().nth(1) {
        Some("--check") => return check(),
        None => 16,
        Some(text) => match text.parse::<usize>() {
            Ok(count) if (1..=4096).contains(&count) => count,
            _ => {
                eprintln!("usage: random [COUNT, 1 to 4096] | random --check");
                return 2;
            }
        },
    };
    let mut bytes = vec![0u8; count];
    omit_rt::system::random(&mut bytes);
    for (index, byte) in bytes.iter().enumerate() {
        print!("{byte:02x}");
        if index % 32 == 31 {
            println!();
        }
    }
    if count % 32 != 0 {
        println!();
    }
    0
}
