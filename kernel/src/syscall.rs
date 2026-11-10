// SPDX-License-Identifier: GPL-2.0-only
//! System call handlers.
//!
//! The ABI (numbers, argument registers, error values) is defined in the
//! `omit_abi` crate. Everything a user program passes in is untrusted:
//! numbers are range-checked and pointers are only ever used through the
//! process's address space, which validates them page by page.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use omit_abi::{Errno, PATH_MAX, READ_MAX, STDERR, STDIN, STDOUT, WRITE_MAX, nr, open};
use omit_shell::LineEditor;
use omit_vfs::{FileType, FsError, WriteMode, path};

use crate::console::{self, Writer};
use crate::process::{self, OpenFile};
use crate::sync::Mutex;
use crate::{fs, kprint, sched, time};

type SysResult = Result<u64, Errno>;

/// Entry point for all system calls; called by the assembly entry code
/// with interrupts enabled, on the calling thread's kernel stack.
pub extern "C" fn dispatch(number: u64, a0: u64, a1: u64, a2: u64, _a3: u64, _a4: u64) -> i64 {
    process::exit_if_killed();
    let result: SysResult = match number {
        nr::EXIT => process::exit(a0 as i32),
        nr::WRITE => write(a0, a1, a2),
        nr::READ => read(a0, a1, a2),
        nr::OPEN => open_file(a0, a1, a2),
        nr::CLOSE => {
            if process::close_file(a0) {
                Ok(0)
            } else {
                Err(Errno::BadFileDescriptor)
            }
        }
        nr::GETPID => Ok(sched::current_pid().unwrap_or(0)),
        nr::YIELD => {
            sched::yield_now();
            Ok(0)
        }
        nr::SLEEP => {
            // Round up so a sleep is never shorter than requested.
            let ticks = a0.saturating_mul(u64::from(time::TICK_HZ)).div_ceil(1000);
            sched::sleep_ticks(ticks);
            Ok(0)
        }
        nr::UPTIME => Ok(time::monotonic_ns() / 1_000_000),
        _ => Err(Errno::NoSuchSyscall),
    };
    process::exit_if_killed();
    match result {
        Ok(value) => value as i64,
        Err(errno) => errno.as_result(),
    }
}

fn errno(error: FsError) -> Errno {
    match error {
        FsError::NotFound => Errno::NotFound,
        FsError::NotADirectory => Errno::NotADirectory,
        FsError::IsADirectory => Errno::IsADirectory,
        FsError::AlreadyExists => Errno::Exists,
        FsError::NoSpace => Errno::NoSpace,
        FsError::DirectoryNotEmpty | FsError::InvalidName | FsError::InvalidArgument => {
            Errno::InvalidArgument
        }
    }
}

/// Copies `length` bytes from user address `address` into a new buffer.
fn copy_in(address: u64, length: usize) -> Result<Vec<u8>, Errno> {
    let mut data = Vec::new();
    data.try_reserve_exact(length).map_err(|_| Errno::NoSpace)?;
    data.resize(length, 0);
    // The scheduler lock is held during each copy, so do it in pieces.
    for (index, chunk) in data.chunks_mut(4096).enumerate() {
        let from = address + (index * 4096) as u64;
        if sched::with_address_space(|space| space.read(from, chunk)) != Some(true) {
            return Err(Errno::Fault);
        }
    }
    Ok(data)
}

/// Copies `data` to user address `address`.
fn copy_out(address: u64, data: &[u8]) -> Result<(), Errno> {
    for (index, chunk) in data.chunks(4096).enumerate() {
        let to = address + (index * 4096) as u64;
        if sched::with_address_space(|space| space.write(to, chunk)) != Some(true) {
            return Err(Errno::Fault);
        }
    }
    Ok(())
}

/// Reads a path argument and resolves it against the working directory.
fn path_argument(address: u64, length: u64) -> Result<String, Errno> {
    if length == 0 || length > PATH_MAX {
        return Err(Errno::InvalidArgument);
    }
    let bytes = copy_in(address, length as usize)?;
    let text = core::str::from_utf8(&bytes).map_err(|_| Errno::InvalidArgument)?;
    let cwd = process::current_dir().ok_or(Errno::InvalidArgument)?;
    path::normalize(&cwd, text).map_err(errno)
}

fn open_file(path_address: u64, path_length: u64, flags: u64) -> SysResult {
    if flags & !open::ALL != 0 {
        return Err(Errno::InvalidArgument);
    }
    let writable = flags & open::WRITE != 0;
    if !writable && flags != open::READ {
        // CREATE, TRUNCATE and APPEND only make sense when writing.
        return Err(Errno::InvalidArgument);
    }
    let path = path_argument(path_address, path_length)?;

    {
        let mut root = fs::root().ok_or(Errno::NotFound)?;
        match root.metadata(&path) {
            Ok(metadata) if metadata.file_type == FileType::Directory => {
                return Err(Errno::IsADirectory);
            }
            Ok(_) => {
                if flags & open::TRUNCATE != 0 {
                    root.write_file(&path, b"", WriteMode::Truncate)
                        .map_err(errno)?;
                }
            }
            Err(FsError::NotFound) if flags & open::CREATE != 0 => {
                root.write_file(&path, b"", WriteMode::Truncate)
                    .map_err(errno)?;
            }
            Err(error) => return Err(errno(error)),
        }
    }

    process::install_file(OpenFile {
        path,
        offset: 0,
        writable,
        append: flags & open::APPEND != 0,
    })
    .ok_or(Errno::TooManyOpenFiles)
}

fn read(fd: u64, address: u64, length: u64) -> SysResult {
    let length = length.min(READ_MAX) as usize;
    if fd == STDIN {
        return read_console(address, length);
    }
    let file = process::file(fd).ok_or(Errno::BadFileDescriptor)?;
    if file.writable {
        return Err(Errno::BadFileDescriptor);
    }
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(length)
        .map_err(|_| Errno::NoSpace)?;
    buffer.resize(length, 0);
    let count = fs::root()
        .ok_or(Errno::NotFound)?
        .read_at(&file.path, file.offset, &mut buffer)
        .map_err(errno)?;
    copy_out(address, &buffer[..count])?;
    process::set_file_offset(fd, file.offset + count as u64);
    Ok(count as u64)
}

/// Line discipline for console reads, shared by all processes so that a
/// partially typed line survives across `read` calls.
static CONSOLE_LINE: Mutex<LineEditor> = Mutex::new(LineEditor::new());

/// End-of-transmission (Ctrl-D): end of input when typed on an empty line.
const END_OF_INPUT: u8 = 0x04;

/// Reads one line from the console, with echo and line editing. Returns 0
/// at end of input (Ctrl-D on an empty line). A line longer than the
/// buffer is truncated.
fn read_console(address: u64, length: usize) -> SysResult {
    if length == 0 {
        return Ok(0);
    }
    let mut line = {
        let mut editor = CONSOLE_LINE.lock();
        loop {
            let Some(byte) = console::read_byte_unless_killed() else {
                // Killed while waiting. Release the line discipline before
                // exiting: `exit` does not return, so the guard would
                // otherwise never be dropped.
                drop(editor);
                process::exit_if_killed();
                return Ok(0);
            };
            if byte == END_OF_INPUT && editor.is_empty() {
                return Ok(0);
            }
            if let Some(line) = editor.feed(byte, &mut Writer) {
                break line;
            }
        }
    };
    line.push('\n');
    let bytes = &line.as_bytes()[..line.len().min(length)];
    copy_out(address, bytes)?;
    Ok(bytes.len() as u64)
}

fn write(fd: u64, address: u64, length: u64) -> SysResult {
    if length > WRITE_MAX {
        return Err(Errno::InvalidArgument);
    }
    if fd == STDOUT || fd == STDERR {
        return write_console(address, length);
    }
    let file = process::file(fd).ok_or(Errno::BadFileDescriptor)?;
    if !file.writable {
        return Err(Errno::BadFileDescriptor);
    }
    let data = copy_in(address, length as usize)?;
    let offset = (!file.append).then_some(file.offset);
    let end = fs::root()
        .ok_or(Errno::NotFound)?
        .write_at(&file.path, offset, &data)
        .map_err(errno)?;
    process::set_file_offset(fd, end);
    Ok(length)
}

fn write_console(address: u64, length: u64) -> SysResult {
    // Copy in bounded pieces: the scheduler lock is held only briefly and
    // nothing from an inaccessible buffer is printed.
    let mut buffer = vec![0u8; 256];
    let mut done = 0u64;
    while done < length {
        let chunk = (length - done).min(buffer.len() as u64) as usize;
        let copied =
            sched::with_address_space(|space| space.read(address + done, &mut buffer[..chunk]));
        if copied != Some(true) {
            return Err(Errno::Fault);
        }
        // Program output is arbitrary bytes; show what is valid text.
        for piece in buffer[..chunk].utf8_chunks() {
            kprint!("{}", piece.valid());
            if !piece.invalid().is_empty() {
                kprint!("\u{fffd}");
            }
        }
        done += chunk as u64;
    }
    Ok(length)
}
