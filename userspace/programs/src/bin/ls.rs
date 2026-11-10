// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Lists directory contents. With `-l`, also shows type and size.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use omit_rt::abi::{Errno, KIND_DIRECTORY};
use omit_rt::{eprintln, println};

omit_rt::entry!(main);

fn join(directory: &str, name: &str) -> String {
    if directory.ends_with('/') {
        format!("{directory}{name}")
    } else {
        format!("{directory}/{name}")
    }
}

fn show(long: bool, is_dir: bool, size: u64, name: &str) {
    if long {
        println!("{} {size:>10} {name}", if is_dir { 'd' } else { '-' });
    } else {
        println!("{name}");
    }
}

fn list(path: &str, long: bool, with_header: bool) -> Result<(), Errno> {
    let info = omit_rt::stat(path)?;
    if info.kind != KIND_DIRECTORY {
        show(long, false, info.size, path);
        return Ok(());
    }
    if with_header {
        println!("{path}:");
    }
    for entry in omit_rt::read_dir(path)? {
        let size = omit_rt::stat(&join(path, &entry.name)).map_or(0, |info| info.size);
        show(long, entry.is_dir, size, &entry.name);
    }
    Ok(())
}

fn main() -> i32 {
    let mut long = false;
    let mut paths: Vec<&str> = Vec::new();
    for argument in omit_rt::args().skip(1) {
        match argument {
            "-l" => long = true,
            path => paths.push(path),
        }
    }
    if paths.is_empty() {
        paths.push(".");
    }

    let mut status = 0;
    for (index, path) in paths.iter().enumerate() {
        if index > 0 && paths.len() > 1 {
            println!();
        }
        if let Err(error) = list(path, long, paths.len() > 1) {
            eprintln!("ls: {path}: {error}");
            status = 1;
        }
    }
    status
}
