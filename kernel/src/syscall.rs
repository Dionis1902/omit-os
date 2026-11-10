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

use omit_abi::{
    ARGS_MAX, Blit, Errno, INPUT_READ_MAX, INPUT_TIMEOUT_MAX, InputEvent, KIND_DIRECTORY,
    KIND_FILE, PATH_MAX, READ_MAX, READ_NO_WAIT, STDERR, STDIN, STDOUT, Stat, WAIT_NO_HANG,
    WRITE_MAX, nr, open, protect,
};
use omit_shell::LineEditor;
use omit_vfs::{FileType, FsError, WriteMode, path};

use crate::console::{self, Writer};
use crate::pipe::{self, PipeError};
use crate::process::{self, Descriptor, OpenFile};
use crate::sync::Mutex;
use crate::{display, fs, input, kprint, sched, time};

type SysResult = Result<u64, Errno>;

/// Entry point for all system calls; called by the assembly entry code
/// with interrupts enabled, on the calling thread's kernel stack.
pub extern "C" fn dispatch(number: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    process::exit_if_killed();
    let result: SysResult = match number {
        nr::EXIT => process::exit(a0 as i32),
        nr::WRITE => write(a0, a1, a2),
        nr::READ => read(a0, a1, a2, a3),
        nr::OPEN => open_file(a0, a1, a2),
        nr::CLOSE => {
            if process::close(a0) {
                Ok(0)
            } else {
                Err(Errno::BadFileDescriptor)
            }
        }
        nr::MAP => process::map_memory(a0, a1).ok_or(Errno::NoSpace),
        nr::UNMAP => {
            if process::unmap_memory(a0, a1) {
                Ok(0)
            } else {
                Err(Errno::InvalidArgument)
            }
        }
        nr::PROTECT => {
            let writable = a2 & protect::WRITE != 0;
            let executable = a2 & protect::EXECUTE != 0;
            if a2 & !(protect::WRITE | protect::EXECUTE) == 0
                && process::protect_memory(a0, a1, writable, executable)
            {
                Ok(0)
            } else {
                Err(Errno::InvalidArgument)
            }
        }
        nr::SET_THREAD_POINTER => {
            // Only addresses in the user half: loading a non-canonical or
            // kernel address into GS base is never legitimate.
            if a0 == 0 || crate::arch::uspace::is_user_range(a0, 1) {
                sched::set_thread_pointer(a0);
                Ok(0)
            } else {
                Err(Errno::InvalidArgument)
            }
        }
        nr::DISPLAY_INFO => display_info(a0),
        nr::DISPLAY_ACQUIRE => current_pid().and_then(display::acquire).map(|()| 0),
        nr::DISPLAY_RELEASE => current_pid().and_then(|pid| {
            if display::release(pid) {
                Ok(0)
            } else {
                Err(Errno::InvalidArgument)
            }
        }),
        nr::DISPLAY_BLIT => display_blit(a0),
        nr::INPUT_READ => input_read(a0, a1, a2),
        nr::STAT => stat(a0, a1, a2),
        nr::READDIR => read_dir(a0, a1, a2, a3),
        nr::MKDIR => path_argument(a0, a1).and_then(|path| {
            fs::root()
                .ok_or(Errno::NotFound)?
                .create_dir(&path)
                .map_err(errno)
                .map(|()| 0)
        }),
        nr::REMOVE => path_argument(a0, a1).and_then(|path| {
            fs::root()
                .ok_or(Errno::NotFound)?
                .remove(&path)
                .map_err(errno)
                .map(|()| 0)
        }),
        nr::RENAME => {
            let from = path_argument(a0, a1);
            let to = path_argument(a2, a3);
            from.and_then(|from| {
                fs::root()
                    .ok_or(Errno::NotFound)?
                    .rename(&from, &to?)
                    .map_err(errno)
                    .map(|()| 0)
            })
        }
        nr::SPAWN => spawn(a0, a1, a2, a3, a4),
        nr::WAIT if a1 == WAIT_NO_HANG => match process::try_wait(a0) {
            Some(Some(status)) => Ok(status as u32 as u64),
            Some(None) => Err(Errno::Busy),
            None => Err(Errno::NoSuchProcess),
        },
        nr::WAIT if a1 != 0 => Err(Errno::InvalidArgument),
        nr::WAIT => process::wait(a0)
            .map(|status| status as u32 as u64)
            .ok_or(Errno::NoSuchProcess),
        nr::CLOCK => Ok(time::monotonic_ns() / 1000),
        nr::PIPE => create_pipe(a0),
        nr::KILL => {
            if sched::kill(a0) {
                Ok(0)
            } else {
                Err(Errno::NoSuchProcess)
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
        FsError::DirectoryNotEmpty => Errno::NotEmpty,
        FsError::Io => Errno::Io,
        FsError::Busy => Errno::Busy,
        FsError::CrossDevice => Errno::CrossDevice,
        FsError::InvalidName | FsError::InvalidArgument => Errno::InvalidArgument,
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

    process::install(Descriptor::File(OpenFile {
        path,
        offset: 0,
        writable,
        append: flags & open::APPEND != 0,
    }))
    .map_err(|_| Errno::TooManyOpenFiles)
}

fn stat(path_address: u64, path_length: u64, out: u64) -> SysResult {
    let path = path_argument(path_address, path_length)?;
    let metadata = fs::root()
        .ok_or(Errno::NotFound)?
        .metadata(&path)
        .map_err(errno)?;
    let kind = match metadata.file_type {
        FileType::File => KIND_FILE,
        FileType::Directory => KIND_DIRECTORY,
    };
    // The layout of `omit_abi::Stat`: two little-endian 64-bit fields.
    let mut bytes = [0u8; size_of::<Stat>()];
    bytes[..8].copy_from_slice(&kind.to_le_bytes());
    bytes[8..].copy_from_slice(&metadata.size.to_le_bytes());
    copy_out(out, &bytes)?;
    Ok(0)
}

fn read_dir(path_address: u64, path_length: u64, buffer: u64, buffer_length: u64) -> SysResult {
    let path = path_argument(path_address, path_length)?;
    let entries = fs::root()
        .ok_or(Errno::NotFound)?
        .read_dir(&path)
        .map_err(errno)?;
    let mut records = Vec::new();
    for entry in entries {
        records
            .try_reserve(entry.name.len() + 2)
            .map_err(|_| Errno::NoSpace)?;
        records.push(match entry.file_type {
            FileType::File => KIND_FILE as u8,
            FileType::Directory => KIND_DIRECTORY as u8,
        });
        records.extend_from_slice(entry.name.as_bytes());
        records.push(0);
    }
    if records.len() as u64 <= buffer_length {
        copy_out(buffer, &records)?;
    }
    Ok(records.len() as u64)
}

fn create_pipe(address: u64) -> SysResult {
    let (reader, writer) = pipe::new();
    let read_fd =
        process::install(Descriptor::PipeRead(reader)).map_err(|_| Errno::TooManyOpenFiles)?;
    let write_fd = match process::install(Descriptor::PipeWrite(writer)) {
        Ok(fd) => fd,
        Err(_) => {
            process::close(read_fd);
            return Err(Errno::TooManyOpenFiles);
        }
    };
    let mut ends = [0u8; 16];
    ends[..8].copy_from_slice(&read_fd.to_le_bytes());
    ends[8..].copy_from_slice(&write_fd.to_le_bytes());
    if let Err(error) = copy_out(address, &ends) {
        process::close(read_fd);
        process::close(write_fd);
        return Err(error);
    }
    Ok(0)
}

fn current_pid() -> Result<u64, Errno> {
    sched::current_pid().ok_or(Errno::InvalidArgument)
}

/// Views a plain-data value as bytes, for copying it to user memory.
fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: only used with the `repr(C)` ABI structures, which consist
    // of integers without padding; any such value is readable as bytes
    // for its full size.
    unsafe { core::slice::from_raw_parts(core::ptr::from_ref(value).cast(), size_of::<T>()) }
}

fn display_info(address: u64) -> SysResult {
    let info = display::info().ok_or(Errno::NotFound)?;
    copy_out(address, bytes_of(&info))?;
    Ok(0)
}

fn display_blit(address: u64) -> SysResult {
    let raw = copy_in(address, size_of::<Blit>())?;
    let field = |index: usize| {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&raw[8 + index * 4..12 + index * 4]);
        u32::from_le_bytes(bytes)
    };
    let mut pixels = [0u8; 8];
    pixels.copy_from_slice(&raw[..8]);
    let request = Blit {
        pixels: u64::from_le_bytes(pixels),
        stride: field(0),
        x: field(1),
        y: field(2),
        width: field(3),
        height: field(4),
    };
    display::blit(current_pid()?, &request, |from, row| {
        sched::with_address_space(|space| space.read(from, row)) == Some(true)
    })?;
    Ok(0)
}

fn input_read(address: u64, capacity: u64, timeout_ms: u64) -> SysResult {
    if !display::is_owner(current_pid()?) {
        return Err(Errno::Busy);
    }
    if timeout_ms > INPUT_TIMEOUT_MAX {
        return Err(Errno::InvalidArgument);
    }
    let capacity = capacity.min(INPUT_READ_MAX) as usize;
    // Rounded up, so that a short timeout still waits.
    let tick_ms = 1000 / u64::from(time::TICK_HZ);
    input::wait(timeout_ms.div_ceil(tick_ms));
    process::exit_if_killed();
    let mut events = Vec::new();
    while events.len() < capacity * size_of::<InputEvent>() {
        match input::pop() {
            Some(event) => events.extend_from_slice(bytes_of(&event)),
            None => break,
        }
    }
    copy_out(address, &events)?;
    Ok((events.len() / size_of::<InputEvent>()) as u64)
}

fn spawn(
    path_address: u64,
    path_length: u64,
    args_address: u64,
    args_length: u64,
    stdio_request: u64,
) -> SysResult {
    let path = path_argument(path_address, path_length)?;
    if args_length > ARGS_MAX as u64 {
        return Err(Errno::InvalidArgument);
    }
    let raw = copy_in(args_address, args_length as usize)?;
    let mut args = Vec::new();
    for piece in raw.split(|&byte| byte == 0) {
        if !piece.is_empty() {
            let text = core::str::from_utf8(piece).map_err(|_| Errno::InvalidArgument)?;
            args.push(String::from(text));
        }
    }

    let mut image = fs::root()
        .ok_or(Errno::NotFound)?
        .read_file(&path)
        .map_err(errno)?;
    if image.starts_with(omit_shell::WINDOWS_MAGIC) {
        // A Windows executable: run the Windows program loader on it, as
        // the shell does.
        image = fs::root()
            .ok_or(Errno::NotFound)?
            .read_file(omit_shell::WINDOWS_LOADER)
            .map_err(|_| Errno::NotExecutable)?;
        if args.is_empty() {
            args.push(String::new());
        }
        args[0] = omit_shell::WINDOWS_LOADER.into();
        args.insert(1, path.clone());
    }
    let cwd = process::current_dir().ok_or(Errno::InvalidArgument)?;
    let name = path::file_name(&path).unwrap_or("program");
    // Each stream: the descriptor named by the caller, or the caller's own
    // stream of the same kind.
    let mut stdio = process::stdio();
    for (index, shift) in [(0usize, 16u32), (1, 0), (2, 32)] {
        let Some(fd) = ((stdio_request >> shift) & 0xFFFF).checked_sub(1) else {
            continue;
        };
        let input = index == 0;
        stdio[index] = Some(match process::descriptor(fd) {
            Some(Descriptor::File(file)) if file.writable != input => {
                Descriptor::File(OpenFile {
                    path: file.path,
                    offset: 0,
                    writable: !input,
                    // Output is added to what the file holds: several
                    // programs can share a log this way.
                    append: !input,
                })
            }
            Some(pipe @ Descriptor::PipeRead(_)) if input => pipe,
            Some(pipe @ Descriptor::PipeWrite(_)) if !input => pipe,
            _ => return Err(Errno::BadFileDescriptor),
        });
    }
    process::spawn(name, &image, &args, &cwd, stdio).map_err(|error| match error {
        process::ExecError::Elf(_) | process::ExecError::Unsupported(_) => Errno::NotExecutable,
        process::ExecError::ArgumentsTooLong => Errno::InvalidArgument,
        process::ExecError::Map(_) | process::ExecError::Spawn(_) => Errno::NoSpace,
    })
}

fn read(fd: u64, address: u64, length: u64, flags: u64) -> SysResult {
    if flags & !READ_NO_WAIT != 0 {
        return Err(Errno::InvalidArgument);
    }
    let no_wait = flags & READ_NO_WAIT != 0;
    let length = length.min(READ_MAX) as usize;
    let mut buffer = Vec::new();
    let count = match process::descriptor(fd) {
        // The console's line editing has no way to report "nothing yet".
        None if fd == STDIN && no_wait => return Err(Errno::InvalidArgument),
        None if fd == STDIN => return read_console(address, length),
        Some(Descriptor::File(file)) if !file.writable => {
            buffer
                .try_reserve_exact(length)
                .map_err(|_| Errno::NoSpace)?;
            buffer.resize(length, 0);
            let count = fs::root()
                .ok_or(Errno::NotFound)?
                .read_at(&file.path, file.offset, &mut buffer)
                .map_err(errno)?;
            process::set_offset(fd, file.offset + count as u64);
            count
        }
        Some(Descriptor::PipeRead(pipe)) => {
            buffer
                .try_reserve_exact(length)
                .map_err(|_| Errno::NoSpace)?;
            buffer.resize(length, 0);
            if no_wait {
                let count = pipe.try_read(&mut buffer).ok_or(Errno::WouldBlock)?;
                copy_out(address, &buffer[..count])?;
                return Ok(count as u64);
            }
            match pipe.read(&mut buffer) {
                Ok(count) => count,
                Err(_) => {
                    // Killed while waiting. Let go of the pipe end first:
                    // `exit` does not return.
                    drop(pipe);
                    process::exit_if_killed();
                    0
                }
            }
        }
        _ => return Err(Errno::BadFileDescriptor),
    };
    copy_out(address, &buffer[..count])?;
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
    match process::descriptor(fd) {
        // Standard output and error go to the console unless the process
        // was started with them connected elsewhere.
        None if fd == STDOUT || fd == STDERR => write_console(address, length),
        Some(Descriptor::File(file)) if file.writable => {
            let data = copy_in(address, length as usize)?;
            let offset = (!file.append).then_some(file.offset);
            let end = fs::root()
                .ok_or(Errno::NotFound)?
                .write_at(&file.path, offset, &data)
                .map_err(errno)?;
            process::set_offset(fd, end);
            Ok(length)
        }
        Some(Descriptor::PipeWrite(pipe)) => {
            let data = copy_in(address, length as usize)?;
            match pipe.write(&data) {
                Ok(()) => Ok(length),
                Err(PipeError::NoReaders) => Err(Errno::BrokenPipe),
                Err(PipeError::Killed) => {
                    drop(pipe);
                    process::exit_if_killed();
                    Err(Errno::BrokenPipe)
                }
            }
        }
        _ => Err(Errno::BadFileDescriptor),
    }
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
