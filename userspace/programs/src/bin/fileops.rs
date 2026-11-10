// SPDX-License-Identifier: GPL-2.0-only
//! Self-checking test of the directory system calls: creates, renames and
//! removes files and directories under the directory given as argument and
//! verifies every step, including the errors that must be reported.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use omit_rt::abi::{Errno, KIND_DIRECTORY, KIND_FILE, open};
use omit_rt::println;

omit_rt::entry!(main);

fn check(what: &str, ok: bool) -> Result<(), ()> {
    if ok {
        Ok(())
    } else {
        println!("fileops: FAILED: {what}");
        Err(())
    }
}

fn run(base: &str) -> Result<(), ()> {
    let dir = format!("{base}/work");
    let file = format!("{dir}/data.txt");
    let moved = format!("{dir}/moved.txt");

    check("mkdir", omit_rt::mkdir(&dir).is_ok())?;
    check("mkdir twice", omit_rt::mkdir(&dir) == Err(Errno::Exists))?;
    check(
        "stat directory",
        omit_rt::stat(&dir).map(|s| s.kind) == Ok(KIND_DIRECTORY),
    )?;

    let fd = omit_rt::open(&file, open::WRITE | open::CREATE).map_err(|_| ());
    check("create file", fd.is_ok())?;
    let fd = fd?;
    check("write", omit_rt::write_all(fd, b"twelve bytes").is_ok())?;
    check("close", omit_rt::close(fd).is_ok())?;
    check(
        "close twice",
        omit_rt::close(fd) == Err(Errno::BadFileDescriptor),
    )?;
    let info = omit_rt::stat(&file);
    check(
        "stat file",
        info.map(|s| (s.kind, s.size)) == Ok((KIND_FILE, 12)),
    )?;

    check("rename", omit_rt::rename(&file, &moved).is_ok())?;
    check(
        "old name gone",
        omit_rt::stat(&file) == Err(Errno::NotFound),
    )?;
    let names: Vec<_> = omit_rt::read_dir(&dir)
        .map_err(|_| ())?
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    check("listing", names == ["moved.txt"])?;

    check(
        "remove non-empty directory",
        omit_rt::remove(&dir) == Err(Errno::NotEmpty),
    )?;
    check("remove file", omit_rt::remove(&moved).is_ok())?;
    check("remove directory", omit_rt::remove(&dir).is_ok())?;
    check(
        "directory gone",
        omit_rt::stat(&dir) == Err(Errno::NotFound),
    )?;
    check(
        "read_dir of a missing directory",
        omit_rt::read_dir(&dir).err() == Some(Errno::NotFound),
    )?;
    Ok(())
}

fn main() -> i32 {
    let base = omit_rt::args().nth(1).unwrap_or("/");
    match run(base.trim_end_matches('/')) {
        Ok(()) => {
            println!("fileops: all checks passed");
            0
        }
        Err(()) => 1,
    }
}
