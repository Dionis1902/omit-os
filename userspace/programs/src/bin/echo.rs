// SPDX-License-Identifier: GPL-2.0-only
//! Prints its arguments separated by spaces.

#![no_std]
#![no_main]

use omit_rt::{print, println};

omit_rt::entry!(main);

fn main() -> i32 {
    for (index, argument) in omit_rt::args().skip(1).enumerate() {
        if index > 0 {
            print!(" ");
        }
        print!("{argument}");
    }
    println!();
    0
}
