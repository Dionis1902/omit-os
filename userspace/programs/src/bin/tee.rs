// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Copies standard input to standard output and to the named file.

#![no_std]
#![no_main]

use omit_rt::abi::{STDIN, STDOUT, open};
use omit_rt::eprintln;

omit_rt::entry!(main);

fn main() -> i32 {
    let Some(path) = omit_rt::args().nth(1) else {
        eprintln!("usage: tee FILE");
        return 2;
    };
    let file = match omit_rt::open(path, open::WRITE | open::CREATE | open::TRUNCATE) {
        Ok(fd) => fd,
        Err(error) => {
            eprintln!("tee: {path}: {error}");
            return 1;
        }
    };
    let mut buffer = [0u8; 1024];
    loop {
        match omit_rt::read(STDIN, &mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let data = &buffer[..count];
                if omit_rt::write_all(STDOUT, data).is_err()
                    || omit_rt::write_all(file, data).is_err()
                {
                    eprintln!("tee: write error");
                    return 1;
                }
            }
            Err(error) => {
                eprintln!("tee: standard input: {error}");
                return 1;
            }
        }
    }
    let _ = omit_rt::close(file);
    0
}
