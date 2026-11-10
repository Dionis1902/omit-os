// SPDX-License-Identifier: GPL-2.0-only
//! `pipetest` — tests pipes between processes.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use omit_rt::abi::Errno;
use omit_rt::println;

omit_rt::entry!(main);

/// Reads from `fd` until end of input.
fn read_all(fd: u64) -> Result<Vec<u8>, Errno> {
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 8192];
    loop {
        match omit_rt::read(fd, &mut chunk)? {
            0 => return Ok(data),
            count => data.extend_from_slice(&chunk[..count]),
        }
    }
}

fn run() -> Result<(), &'static str> {
    // Within one process.
    let (read_end, write_end) = omit_rt::pipe().map_err(|_| "cannot create a pipe")?;
    omit_rt::write_all(write_end, b"through the pipe").map_err(|_| "write failed")?;
    let mut buffer = [0u8; 64];
    let count = omit_rt::read(read_end, &mut buffer).map_err(|_| "read failed")?;
    if &buffer[..count] != b"through the pipe" {
        return Err("data read back differs from data written");
    }
    // The ends are one-way.
    if omit_rt::write(read_end, b"x") != Err(Errno::BadFileDescriptor)
        || omit_rt::read(write_end, &mut buffer) != Err(Errno::BadFileDescriptor)
    {
        return Err("a pipe end worked in the wrong direction");
    }
    let _ = omit_rt::close(write_end);
    if omit_rt::read(read_end, &mut buffer) != Ok(0) {
        return Err("no end of input after the writing end was closed");
    }
    let _ = omit_rt::close(read_end);

    // Writing with no reader left fails instead of blocking for ever.
    let (read_end, write_end) = omit_rt::pipe().map_err(|_| "cannot create a pipe")?;
    let _ = omit_rt::close(read_end);
    if omit_rt::write(write_end, b"x") != Err(Errno::BrokenPipe) {
        return Err("writing to a pipe without readers did not fail");
    }
    let _ = omit_rt::close(write_end);

    // The output of another program.
    let (read_end, write_end) = omit_rt::pipe().map_err(|_| "cannot create a pipe")?;
    let child = omit_rt::spawn_with_stdio(
        "/bin/echo",
        &["echo", "sent", "by", "echo"],
        None,
        Some(write_end),
        None,
    )
    .map_err(|_| "cannot start echo")?;
    // Only the child must hold the writing end, or the end never comes.
    let _ = omit_rt::close(write_end);
    let output = read_all(read_end).map_err(|_| "reading echo's output failed")?;
    let _ = omit_rt::close(read_end);
    if output != b"sent by echo\n" {
        return Err("echo's output did not arrive intact");
    }
    if omit_rt::wait(child) != Ok(0) {
        return Err("echo did not exit normally");
    }

    // A filter: this program -> cat -> this program, with far more data
    // than a pipe holds, so that both sides have to wait for each other.
    let (cat_in_read, cat_in_write) = omit_rt::pipe().map_err(|_| "cannot create a pipe")?;
    let (cat_out_read, cat_out_write) = omit_rt::pipe().map_err(|_| "cannot create a pipe")?;
    let cat = omit_rt::spawn_with_stdio(
        "/bin/cat",
        &["cat"],
        Some(cat_in_read),
        Some(cat_out_write),
        None,
    )
    .map_err(|_| "cannot start cat")?;
    let _ = omit_rt::close(cat_in_read);
    let _ = omit_rt::close(cat_out_write);

    const TOTAL: usize = 300_000;
    const CHUNK: usize = 5000;
    let pattern = |index: usize| (index % 253) as u8;
    let mut sent = 0;
    let mut received = Vec::with_capacity(TOTAL);
    let mut chunk = vec![0u8; 8192];
    // Alternate between sending a piece and collecting what has come
    // back, so that neither pipe stays full.
    while sent < TOTAL {
        let piece: Vec<u8> = (sent..(sent + CHUNK).min(TOTAL)).map(pattern).collect();
        omit_rt::write_all(cat_in_write, &piece).map_err(|_| "writing to cat failed")?;
        sent += piece.len();
        while received.len() + 2 * CHUNK < sent {
            let count =
                omit_rt::read(cat_out_read, &mut chunk).map_err(|_| "reading from cat failed")?;
            if count == 0 {
                return Err("cat ended its output early");
            }
            received.extend_from_slice(&chunk[..count]);
        }
    }
    let _ = omit_rt::close(cat_in_write);
    received.extend(read_all(cat_out_read).map_err(|_| "reading from cat failed")?);
    let _ = omit_rt::close(cat_out_read);
    if received.len() != TOTAL || received.iter().enumerate().any(|(i, &b)| b != pattern(i)) {
        return Err("data was changed on its way through cat");
    }
    if omit_rt::wait(cat) != Ok(0) {
        return Err("cat did not exit normally");
    }
    Ok(())
}

fn main() -> i32 {
    match run() {
        Ok(()) => {
            println!("pipetest: all checks passed");
            0
        }
        Err(what) => {
            println!("pipetest: FAILED: {what}");
            1
        }
    }
}
