// SPDX-License-Identifier: GPL-2.0-only
//! Self-checking test of the directory system calls: creates, renames and
//! removes files and directories under the directory given as argument and
//! verifies every step, including the errors that must be reported.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use omit_rt::abi::{Errno, KIND_DIRECTORY, KIND_FILE, open, seek};
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

    // A file open for reading and writing, with the position moved
    // about: overwrite in the middle, read back, extend past the end.
    let fd = omit_rt::open(&file, open::READ_WRITE).map_err(|_| ());
    check("open for reading and writing", fd.is_ok())?;
    let fd = fd?;
    let mut buffer = [0u8; 16];
    check(
        "read from the start",
        omit_rt::read(fd, &mut buffer[..6]) == Ok(6) && &buffer[..6] == b"twelve",
    )?;
    check(
        "seek from the start",
        omit_rt::seek(fd, 7, seek::START) == Ok(7),
    )?;
    check("overwrite", omit_rt::write_all(fd, b"BYTES").is_ok())?;
    check(
        "the position follows the write",
        omit_rt::seek(fd, 0, seek::CURRENT) == Ok(12),
    )?;
    check(
        "seek from the end",
        omit_rt::seek(fd, -12, seek::END) == Ok(0),
    )?;
    check(
        "read back what was written",
        omit_rt::read(fd, &mut buffer) == Ok(12) && &buffer[..12] == b"twelve BYTES",
    )?;
    check(
        "seek before the start",
        omit_rt::seek(fd, -13, seek::END) == Err(Errno::InvalidArgument)
            && omit_rt::seek(fd, 0, 3) == Err(Errno::InvalidArgument),
    )?;
    // Past the end: nothing to read; a write there fills the gap with
    // zeros.
    check(
        "seek past the end",
        omit_rt::seek(fd, 15, seek::START) == Ok(15),
    )?;
    check("read past the end", omit_rt::read(fd, &mut buffer) == Ok(0))?;
    check("write past the end", omit_rt::write_all(fd, b"!").is_ok())?;
    check(
        "seek relative",
        omit_rt::seek(fd, -4, seek::CURRENT) == Ok(12),
    )?;
    check(
        "the gap reads as zeros",
        omit_rt::read(fd, &mut buffer) == Ok(4) && &buffer[..4] == b"\0\0\0!",
    )?;
    check("close", omit_rt::close(fd).is_ok())?;
    check(
        "stat after extending",
        omit_rt::stat(&file).map(|s| s.size) == Ok(16),
    )?;
    // The flags that contradict each other, and things that are not
    // files.
    check(
        "write-only and read-write together",
        omit_rt::open(&file, open::WRITE | open::READ_WRITE) == Err(Errno::InvalidArgument),
    )?;
    check(
        "seek on standard output",
        omit_rt::seek(1, 0, seek::START) == Err(Errno::BadFileDescriptor),
    )?;
    // Read-only and write-only descriptors stay one-way.
    let read_only = omit_rt::open(&file, open::READ).map_err(|_| ())?;
    let write_only = omit_rt::open(&file, open::WRITE).map_err(|_| ())?;
    check(
        "one-way descriptors",
        omit_rt::write(read_only, b"x") == Err(Errno::BadFileDescriptor)
            && omit_rt::read(write_only, &mut buffer) == Err(Errno::BadFileDescriptor),
    )?;
    let _ = omit_rt::close(read_only);
    let _ = omit_rt::close(write_only);

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
