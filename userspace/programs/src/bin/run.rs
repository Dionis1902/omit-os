// SPDX-License-Identifier: GPL-2.0-only
//! Starts each program named on the command line, one after another,
//! waits for it and reports its exit status.

#![no_std]
#![no_main]

extern crate alloc;

use omit_rt::{eprintln, println};

omit_rt::entry!(main);

fn main() -> i32 {
    let mut failures = 0;
    for program in omit_rt::args().skip(1) {
        match omit_rt::spawn(program, &[program]).and_then(omit_rt::wait) {
            Ok(status) => {
                println!("run: {program} exited with status {status}");
                if status != 0 {
                    failures += 1;
                }
            }
            Err(error) => {
                eprintln!("run: {program}: {error}");
                failures += 1;
            }
        }
    }
    failures
}
