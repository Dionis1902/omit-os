// SPDX-License-Identifier: GPL-2.0-only
//! `ipctest` — tests shared memory, channels and `poll`.
//!
//! Run without arguments it performs the checks, starting itself again
//! as a second process (`ipctest shared TOKEN`, `ipctest client`) for
//! the parts that need two.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec;

use omit_rt::abi::{Errno, POLL_MAX, protect};
use omit_rt::println;

omit_rt::entry!(main);

const SELF: &str = "/bin/ipctest";
const CHANNEL: &str = "ipctest";
const LENGTH: usize = 3 * 4096 + 100;

/// The other process of the shared memory check: maps the segment,
/// verifies what the first process wrote and answers in the same memory.
fn shared_child(token: u64) -> i32 {
    let Ok((address, length)) = omit_rt::shared_map(token) else {
        return 2;
    };
    // SAFETY: the kernel mapped `length` bytes at `address`; the parent
    // only reads the memory again after this process has ended.
    let bytes = unsafe { core::slice::from_raw_parts_mut(address, length) };
    if length != LENGTH.next_multiple_of(4096) || bytes[0] != 0xA5 || bytes[LENGTH - 1] != 0x5A {
        return 3;
    }
    bytes[1] = 0x77;
    0
}

/// The other process of the channel check.
fn channel_client() -> i32 {
    let Ok(connection) = omit_rt::channel_connect(CHANNEL) else {
        return 2;
    };
    if omit_rt::write_all(connection, b"question").is_err() {
        return 3;
    }
    let mut buffer = [0u8; 16];
    match omit_rt::read(connection, &mut buffer) {
        Ok(6) if &buffer[..6] == b"answer" => 0,
        _ => 4,
    }
    // The connection is closed by exiting.
}

fn shared_memory() -> Result<(), &'static str> {
    if omit_rt::shared_create(0).is_ok() || omit_rt::shared_map(0x1234_5678).is_ok() {
        return Err("an empty segment or an invented token was accepted");
    }
    let (first, token) = omit_rt::shared_create(LENGTH).map_err(|_| "cannot create a segment")?;
    // Mapped twice in this process: two views of the same memory.
    let (second, length) = omit_rt::shared_map(token).map_err(|_| "cannot map by token")?;
    if length != LENGTH.next_multiple_of(4096) || first == second {
        return Err("the second mapping has the wrong size or address");
    }
    // SAFETY: both mappings are `length` bytes; they are accessed
    // through raw pointers only, since they alias each other.
    unsafe {
        if (0..length).any(|offset| first.add(offset).read_volatile() != 0) {
            return Err("a new segment is not zero-filled");
        }
        first.write_volatile(0xA5);
        first.add(LENGTH - 1).write_volatile(0x5A);
        if second.read_volatile() != 0xA5 || second.add(LENGTH - 1).read_volatile() != 0x5A {
            return Err("a write through one mapping is not visible through the other");
        }
    }
    // Shared memory is unmapped whole, and stays plain data.
    // SAFETY: these calls are expected to fail and change nothing.
    unsafe {
        if omit_rt::unmap(second, 4096).is_ok() || omit_rt::unmap(second.add(4096), 4096).is_ok() {
            return Err("part of a shared mapping was unmapped");
        }
        if omit_rt::protect(second, length, protect::EXECUTE).is_ok() {
            return Err("shared memory was made executable");
        }
    }

    // Another process sees the same memory.
    let argument = format!("{token}");
    let child = omit_rt::spawn(SELF, &["ipctest", "shared", &argument])
        .map_err(|_| "cannot start the second process")?;
    if omit_rt::wait(child) != Ok(0) {
        return Err("the second process did not find the shared data");
    }
    // SAFETY: as above.
    if unsafe { first.add(1).read_volatile() } != 0x77 {
        return Err("the second process's write is not visible");
    }

    // The segment lives while it is mapped, and no longer.
    // SAFETY: the mappings are not used after being unmapped.
    unsafe {
        omit_rt::unmap(second, length).map_err(|_| "cannot unmap a shared mapping")?;
        if first.read_volatile() != 0xA5 {
            return Err("unmapping one view damaged the other");
        }
        let (third, _) = omit_rt::shared_map(token).map_err(|_| "cannot map again")?;
        omit_rt::unmap(third, length).map_err(|_| "cannot unmap")?;
        omit_rt::unmap(first, length).map_err(|_| "cannot unmap the first mapping")?;
    }
    if omit_rt::shared_map(token) != Err(Errno::NotFound) {
        return Err("a segment outlived its last mapping");
    }
    Ok(())
}

fn channels() -> Result<(), &'static str> {
    if omit_rt::channel_connect(CHANNEL) != Err(Errno::NotFound) {
        return Err("connected to a name nobody listens on");
    }
    if omit_rt::channel_listen("with space") != Err(Errno::InvalidArgument)
        || omit_rt::channel_listen("") != Err(Errno::InvalidArgument)
    {
        return Err("an invalid channel name was accepted");
    }
    let listener = omit_rt::channel_listen(CHANNEL).map_err(|_| "cannot listen")?;
    if omit_rt::channel_listen(CHANNEL) != Err(Errno::Exists) {
        return Err("two listeners on one name");
    }
    if omit_rt::channel_accept_no_wait(listener) != Ok(None) {
        return Err("a connection appeared from nowhere");
    }

    // Nothing to wait for: `poll` returns 0 when the time has passed.
    let start = omit_rt::uptime_ms();
    if omit_rt::poll(&[listener], 50, false) != Ok(0) {
        return Err("poll reported a quiet listener as ready");
    }
    if omit_rt::uptime_ms() - start < 40 {
        return Err("poll returned before its timeout");
    }
    if omit_rt::poll(&[listener, 31], 0, false) != Err(Errno::BadFileDescriptor)
        || omit_rt::poll(&vec![listener; POLL_MAX as usize + 1], 0, false)
            != Err(Errno::InvalidArgument)
        // Input events are for the program that owns the display.
        || omit_rt::poll(&[], 0, true) != Err(Errno::Busy)
    {
        return Err("poll accepted invalid arguments");
    }

    // A client in another process: `poll` wakes when it connects, and
    // again when it has written.
    let child =
        omit_rt::spawn(SELF, &["ipctest", "client"]).map_err(|_| "cannot start a client")?;
    if omit_rt::poll(&[listener], 10_000, false) != Ok(1) {
        return Err("poll did not report the connection");
    }
    let connection = omit_rt::channel_accept(listener).map_err(|_| "cannot accept")?;
    if omit_rt::poll(&[listener, connection], 10_000, false) != Ok(0b10) {
        return Err("poll did not report the client's data");
    }
    let mut buffer = [0u8; 16];
    if omit_rt::read(connection, &mut buffer) != Ok(8) || &buffer[..8] != b"question" {
        return Err("the client's data did not arrive");
    }
    if omit_rt::read_no_wait(connection, &mut buffer) != Ok(None) {
        return Err("a read without waiting returned data that was never sent");
    }
    if omit_rt::write_no_wait(connection, b"answer") != Ok(true) {
        return Err("cannot answer the client");
    }
    if omit_rt::wait(child) != Ok(0) {
        return Err("the client did not receive the answer");
    }
    // The client is gone: that is readable as the end of input, and
    // writing fails.
    if omit_rt::poll(&[connection], 10_000, false) != Ok(1)
        || omit_rt::read(connection, &mut buffer) != Ok(0)
    {
        return Err("the end of the connection was not reported");
    }
    if omit_rt::write_no_wait(connection, b"x") != Err(Errno::BrokenPipe) {
        return Err("writing to a closed connection did not fail");
    }
    let _ = omit_rt::close(connection);

    // Writing without waiting stops when the buffer is full instead of
    // blocking, and writes nothing then.
    let (read_end, write_end) = omit_rt::pipe().map_err(|_| "cannot create a pipe")?;
    let block = [7u8; 4096];
    let mut written = 0usize;
    while omit_rt::write_no_wait(write_end, &block) == Ok(true) {
        written += block.len();
        if written > (1 << 20) {
            return Err("a pipe accepted data without limit");
        }
    }
    let mut drained = 0usize;
    let mut chunk = vec![0u8; 8192];
    while let Ok(Some(count)) = omit_rt::read_no_wait(read_end, &mut chunk) {
        drained += count;
    }
    if written == 0 || drained != written {
        return Err("a refused write left data in the pipe");
    }
    let _ = omit_rt::close(read_end);
    let _ = omit_rt::close(write_end);

    // Closing the listener frees the name.
    let _ = omit_rt::close(listener);
    if omit_rt::channel_connect(CHANNEL) != Err(Errno::NotFound) {
        return Err("the name outlived its listener");
    }
    let again = omit_rt::channel_listen(CHANNEL).map_err(|_| "the name was not released")?;
    let _ = omit_rt::close(again);
    Ok(())
}

fn main() -> i32 {
    let mut arguments = omit_rt::args().skip(1);
    match (arguments.next(), arguments.next()) {
        (Some("shared"), Some(token)) => return token.parse().map_or(1, shared_child),
        (Some("client"), _) => return channel_client(),
        _ => {}
    }
    for (name, check) in [
        (
            "shared memory",
            shared_memory as fn() -> Result<(), &'static str>,
        ),
        ("channels and poll", channels),
    ] {
        if let Err(problem) = check() {
            println!("ipctest: FAILED ({name}): {problem}");
            return 1;
        }
    }
    println!("ipctest: all checks passed");
    0
}
