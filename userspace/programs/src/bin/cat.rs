// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Copies files, or standard input if no file is named, to standard
//! output.

#![no_std]
#![no_main]

use omit_rt::abi::{Errno, STDIN, STDOUT, open};
use omit_rt::eprintln;

omit_rt::entry!(main);

fn copy(fd: u64) -> Result<(), Errno> {
    let mut buffer = [0u8; 4096];
    loop {
        match omit_rt::read(fd, &mut buffer)? {
            0 => return Ok(()),
            count => omit_rt::write_all(STDOUT, &buffer[..count])?,
        }
    }
}

fn main() -> i32 {
    let mut status = 0;
    let mut files = omit_rt::args().skip(1).peekable();
    if files.peek().is_none()
        && let Err(error) = copy(STDIN)
    {
        eprintln!("cat: standard input: {error}");
        status = 1;
    }
    for path in files {
        let outcome = omit_rt::open(path, open::READ).and_then(|fd| {
            let outcome = copy(fd);
            let _ = omit_rt::close(fd);
            outcome
        });
        if let Err(error) = outcome {
            eprintln!("cat: {path}: {error}");
            status = 1;
        }
    }
    status
}
