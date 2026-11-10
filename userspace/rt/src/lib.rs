// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runtime support for Omit OS user programs.
//!
//! A program is a `#![no_std]`, `#![no_main]` binary that uses the
//! [`entry!`] macro to name its main function:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! omit_rt::entry!(main);
//!
//! fn main() -> i32 {
//!     omit_rt::println!("hello");
//!     0
//! }
//! ```
//!
//! The runtime provides the `_start` entry point, safe wrappers for the
//! system calls, `print!`/`println!`, and a panic handler that reports the
//! panic and exits with status 101. Programs can read their arguments,
//! standard input and files, list and modify directories, start other
//! programs, and use the `alloc` crate (`Box`, `Vec`, `String`, ...): the
//! runtime provides the global allocator. There is no environment yet.
//!
//! A program can start more threads ([`thread::spawn`]) and share data
//! between them with [`sync::Mutex`]. A panic in any thread ends the
//! whole program.

#![no_std]

extern crate alloc;

mod heap;
pub mod net;
pub mod sync;
pub mod system;
pub mod thread;
pub mod window;

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use core::ffi::{CStr, c_char};
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};

pub use omit_abi as abi;
use omit_abi::{Errno, nr};

// The kernel starts a process with RSP pointing at the argument block (see
// `args`). Pass that address on, align the stack as the ABI expects at a
// call site, and enter Rust.
global_asm!(
    r#"
.section .text._start, "ax", @progbits
.global _start
_start:
    xor ebp, ebp
    mov rdi, rsp
    and rsp, -16
    call {start}
    ud2
"#,
    start = sym rust_start,
);

unsafe extern "Rust" {
    /// Defined by [`entry!`] in the program.
    fn __omit_main() -> i32;
}

/// Where the kernel left the argument block: the address of `argc`.
static ARGUMENTS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn rust_start(arguments: usize) -> ! {
    ARGUMENTS.store(arguments, Ordering::Relaxed);
    // SAFETY: every program linked against this runtime defines
    // `__omit_main` through `entry!`, with exactly this signature.
    let status = unsafe { __omit_main() };
    exit(status)
}

/// Declares the program's main function, which must be `fn() -> i32`; the
/// value it returns is the process's exit status.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        fn __omit_main() -> i32 {
            let main: fn() -> i32 = $main;
            main()
        }
    };
}

/// Performs a raw system call.
///
/// # Safety
///
/// The arguments must be valid for the system call `number` as documented
/// in the `omit_abi` crate; in particular, pointer arguments must refer to
/// memory the kernel is allowed to read or write on the caller's behalf.
pub unsafe fn syscall(number: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    // SAFETY: forwarded from this function's contract.
    unsafe { syscall4(number, a0, a1, a2, 0) }
}

/// Performs a raw system call with four arguments.
///
/// # Safety
///
/// Same contract as [`syscall`].
pub unsafe fn syscall4(number: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    // SAFETY: forwarded from this function's contract.
    unsafe { syscall5(number, a0, a1, a2, a3, 0) }
}

/// Performs a raw system call with five arguments.
///
/// # Safety
///
/// Same contract as [`syscall`].
pub unsafe fn syscall5(number: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let result: i64;
    // SAFETY: the `syscall` instruction transfers to the kernel, which
    // preserves all registers except those declared here (see the ABI
    // documentation). Validity of the arguments is the caller's contract.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number => result,
            inlateout("rdi") a0 => _,
            inlateout("rsi") a1 => _,
            inlateout("rdx") a2 => _,
            inlateout("r10") a3 => _,
            inlateout("r8") a4 => _,
            lateout("rcx") _, lateout("r9") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    result
}

/// Terminates the process with `status`.
pub fn exit(status: i32) -> ! {
    // SAFETY: `exit` takes a plain integer and does not return.
    unsafe { syscall(nr::EXIT, status as u64, 0, 0) };
    // Not reached; satisfy the type checker without relying on that.
    loop {
        core::hint::spin_loop();
    }
}

/// Writes `bytes` to file descriptor `fd`; returns the number written.
pub fn write(fd: u64, bytes: &[u8]) -> Result<usize, Errno> {
    // SAFETY: the pointer and length describe a live, readable slice.
    result(unsafe { syscall(nr::WRITE, fd, bytes.as_ptr() as u64, bytes.len() as u64) })
        .map(|count| count as usize)
}

/// Writes all of `bytes` to `fd`.
pub fn write_all(fd: u64, mut bytes: &[u8]) -> Result<(), Errno> {
    while !bytes.is_empty() {
        match write(fd, bytes)? {
            0 => return Err(Errno::NoSpace),
            written => bytes = &bytes[written..],
        }
    }
    Ok(())
}

/// The program's arguments, starting with the name it was invoked by.
/// Arguments that are not valid UTF-8 are skipped.
pub fn args() -> impl Iterator<Item = &'static str> {
    let block = ARGUMENTS.load(Ordering::Relaxed) as *const usize;
    let count = if block.is_null() {
        // Only possible before `_start` has run, which `main` never sees.
        0
    } else {
        // SAFETY: the kernel places the argument block on the initial
        // stack: a count, that many pointers to NUL-terminated strings, and
        // a null pointer. It lies above everything the program pushes, so
        // it is valid and unchanged for the life of the process.
        unsafe { *block }
    };
    (0..count).filter_map(move |index| {
        // SAFETY: the argument block (see above) holds `count` pointers
        // after the count itself, and `index < count`.
        let pointer = unsafe { *block.add(1 + index) } as *const c_char;
        // SAFETY: each pointer refers to a NUL-terminated string in the
        // argument block.
        unsafe { CStr::from_ptr(pointer) }.to_str().ok()
    })
}

fn result(value: i64) -> Result<u64, Errno> {
    u64::try_from(value).map_err(|_| Errno::from_result(value).unwrap_or(Errno::InvalidArgument))
}

/// Opens the file at `path`; `flags` are from [`abi::open`]. Returns a
/// file descriptor.
pub fn open(path: &str, flags: u64) -> Result<u64, Errno> {
    // SAFETY: the pointer and length describe a live, readable string.
    result(unsafe { syscall(nr::OPEN, path.as_ptr() as u64, path.len() as u64, flags) })
}

/// Reads up to `buffer.len()` bytes from `fd`; 0 means end of file.
/// Reading standard input returns one line at a time.
pub fn read(fd: u64, buffer: &mut [u8]) -> Result<usize, Errno> {
    // SAFETY: the pointer and length describe a live, writable buffer.
    result(unsafe {
        syscall(
            nr::READ,
            fd,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
        )
    })
    .map(|count| count as usize)
}

/// Like [`read`], but if `fd` is a pipe with nothing in it, returns
/// `Ok(None)` instead of waiting. `Ok(Some(0))` is the end of the input.
pub fn read_no_wait(fd: u64, buffer: &mut [u8]) -> Result<Option<usize>, Errno> {
    // SAFETY: the pointer and length describe a live, writable slice.
    let outcome = result(unsafe {
        syscall4(
            nr::READ,
            fd,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            abi::READ_NO_WAIT,
        )
    });
    match outcome {
        Ok(count) => Ok(Some(count as usize)),
        Err(Errno::WouldBlock) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Like [`write`], but if `fd` is a pipe or channel without room for all
/// of `bytes`, writes nothing and returns `Ok(false)` instead of waiting.
pub fn write_no_wait(fd: u64, bytes: &[u8]) -> Result<bool, Errno> {
    // SAFETY: the pointer and length describe a live, readable slice.
    let outcome = result(unsafe {
        syscall4(
            nr::WRITE,
            fd,
            bytes.as_ptr() as u64,
            bytes.len() as u64,
            abi::WRITE_NO_WAIT,
        )
    });
    match outcome {
        Ok(_) => Ok(true),
        Err(Errno::WouldBlock) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Creates a shared memory segment of `length` bytes, zero-filled, and
/// maps it. Returns its address and the token by which another process
/// can map the same memory with [`shared_map`].
pub fn shared_create(length: usize) -> Result<(*mut u8, u64), Errno> {
    let mut token = 0u64;
    // SAFETY: the pointer refers to a live, writable `u64`.
    let address =
        result(unsafe { syscall(nr::SHARED_CREATE, length as u64, &raw mut token as u64, 0) })?;
    Ok((address as *mut u8, token))
}

/// Maps the shared memory segment named by `token`. Returns its address
/// and length in bytes. Release it with [`unmap`], passing both.
pub fn shared_map(token: u64) -> Result<(*mut u8, usize), Errno> {
    let mut length = 0u64;
    // SAFETY: the pointer refers to a live, writable `u64`.
    let address = result(unsafe { syscall(nr::SHARED_MAP, token, &raw mut length as u64, 0) })?;
    Ok((address as *mut u8, length as usize))
}

/// Starts listening for connections on the channel `name`.
pub fn channel_listen(name: &str) -> Result<u64, Errno> {
    // SAFETY: the pointer and length describe a live, readable string.
    result(unsafe {
        syscall(
            nr::CHANNEL_LISTEN,
            name.as_ptr() as u64,
            name.len() as u64,
            0,
        )
    })
}

/// Takes the next connection made to `listener`, without waiting:
/// `Ok(None)` if there is none.
pub fn channel_accept_no_wait(listener: u64) -> Result<Option<u64>, Errno> {
    // SAFETY: plain integer arguments.
    match result(unsafe { syscall(nr::CHANNEL_ACCEPT, listener, abi::READ_NO_WAIT, 0) }) {
        Ok(fd) => Ok(Some(fd)),
        Err(Errno::WouldBlock) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Waits for the next connection made to `listener`.
pub fn channel_accept(listener: u64) -> Result<u64, Errno> {
    // SAFETY: plain integer arguments.
    result(unsafe { syscall(nr::CHANNEL_ACCEPT, listener, 0, 0) })
}

/// Connects to the listener on the channel `name`. The descriptor can be
/// read and written.
pub fn channel_connect(name: &str) -> Result<u64, Errno> {
    // SAFETY: the pointer and length describe a live, readable string.
    result(unsafe {
        syscall(
            nr::CHANNEL_CONNECT,
            name.as_ptr() as u64,
            name.len() as u64,
            0,
        )
    })
}

/// Waits until one of `fds` (at most [`abi::POLL_MAX`]) can be read
/// without waiting, or `timeout_ms` has passed. Bit *i* of the result is
/// set if `fds[i]` is ready; 0 means the time passed. With `input`, an
/// input event pending for this program (as display owner) also ends the
/// wait and sets [`abi::POLL_INPUT_READY`].
pub fn poll(fds: &[u64], timeout_ms: u64, input: bool) -> Result<u64, Errno> {
    let flags = if input { abi::POLL_INPUT } else { 0 };
    // SAFETY: the pointer and length describe a live, readable slice.
    result(unsafe {
        syscall4(
            nr::POLL,
            fds.as_ptr() as u64,
            fds.len() as u64,
            timeout_ms,
            flags,
        )
    })
}

/// Moves the position of the open file `fd` to `offset` bytes from the
/// place `whence` names (one of [`abi::seek`]); returns the new position
/// from the start of the file.
pub fn seek(fd: u64, offset: i64, whence: u64) -> Result<u64, Errno> {
    // SAFETY: plain integer arguments.
    result(unsafe { syscall(nr::SEEK, fd, offset as u64, whence) })
}

/// Closes file descriptor `fd`.
pub fn close(fd: u64) -> Result<(), Errno> {
    // SAFETY: plain integer argument.
    result(unsafe { syscall(nr::CLOSE, fd, 0, 0) }).map(|_| ())
}

/// Maps `length` bytes of zeroed memory at an address that is a multiple
/// of `align` (a power of two).
pub fn map(length: usize, align: usize) -> Result<*mut u8, Errno> {
    // SAFETY: plain integer arguments.
    result(unsafe { syscall(nr::MAP, length as u64, align as u64, 0) }).map(|addr| addr as *mut u8)
}

/// Releases memory obtained from [`map`].
///
/// # Safety
///
/// Nothing may use the memory afterwards.
pub unsafe fn unmap(address: *mut u8, length: usize) -> Result<(), Errno> {
    // SAFETY: the caller guarantees the range is no longer in use.
    result(unsafe { syscall(nr::UNMAP, address as u64, length as u64, 0) }).map(|_| ())
}

/// Changes the access rights of memory obtained from [`map`]:
/// `protection` is [`abi::protect::READ`], `WRITE` or `EXECUTE`. Memory
/// cannot be writable and executable at once.
///
/// # Safety
///
/// Nothing may rely on the access being taken away: no live reference may
/// point into memory that stops being writable, for example.
pub unsafe fn protect(address: *mut u8, length: usize, protection: u64) -> Result<(), Errno> {
    // SAFETY: the caller guarantees that nothing depends on the old rights.
    result(unsafe { syscall(nr::PROTECT, address as u64, length as u64, protection) }).map(|_| ())
}

/// Sets the base address of the `GS` segment for this thread, for code
/// that expects per-thread data there. Zero clears it.
pub fn set_thread_pointer(address: usize) -> Result<(), Errno> {
    // SAFETY: plain integer argument; the kernel validates it. This
    // runtime does not use `GS` itself.
    result(unsafe { syscall(nr::SET_THREAD_POINTER, address as u64, 0, 0) }).map(|_| ())
}

/// The size of the screen; `Err(NotFound)` if there is none.
pub fn display_info() -> Result<abi::DisplayInfo, Errno> {
    let mut info = abi::DisplayInfo::default();
    // SAFETY: `info` is a live, writable value of the type the kernel
    // fills in.
    result(unsafe { syscall(nr::DISPLAY_INFO, &raw mut info as u64, 0, 0) })?;
    Ok(info)
}

/// Takes over the screen, keyboard and mouse from the text console until
/// [`display_release`] is called or the program ends.
pub fn display_acquire() -> Result<(), Errno> {
    // SAFETY: no arguments.
    result(unsafe { syscall(nr::DISPLAY_ACQUIRE, 0, 0, 0) }).map(|_| ())
}

/// Gives the screen back to the text console.
pub fn display_release() -> Result<(), Errno> {
    // SAFETY: no arguments.
    result(unsafe { syscall(nr::DISPLAY_RELEASE, 0, 0, 0) }).map(|_| ())
}

/// Copies part of an image to the screen. `pixels` holds rows of `stride`
/// pixels (`0x00RRGGBB`); the `width` x `height` rectangle whose top-left
/// corner is at (`x`, `y`) *in the image* is copied to the same place on
/// the screen. This is the natural call for a program that draws into a
/// screen-sized buffer and then shows what changed.
pub fn display_blit(
    pixels: &[u32],
    stride: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Result<(), Errno> {
    if width == 0 || height == 0 {
        return Ok(());
    }
    // The last pixel read must be inside the slice.
    let last = (y + height - 1) * stride + x + width;
    if x + width > stride || last > pixels.len() {
        return Err(Errno::InvalidArgument);
    }
    let request = abi::Blit {
        pixels: pixels[y * stride + x..].as_ptr() as u64,
        stride: stride as u32,
        x: x as u32,
        y: y as u32,
        width: width as u32,
        height: height as u32,
    };
    // SAFETY: `request` is a live value of the type the kernel expects,
    // and the pixel range it describes was checked above to lie inside
    // `pixels`.
    result(unsafe { syscall(nr::DISPLAY_BLIT, &raw const request as u64, 0, 0) }).map(|_| ())
}

/// Waits up to `timeout_ms` milliseconds for keyboard and mouse events
/// and stores them in `events`; returns how many. Zero means the time
/// passed without input. Only works while the program owns the display.
pub fn read_events(events: &mut [abi::InputEvent], timeout_ms: u64) -> Result<usize, Errno> {
    // SAFETY: the pointer and count describe a live, writable slice of
    // the event type; any bit pattern the kernel stores is a valid event.
    result(unsafe {
        syscall(
            nr::INPUT_READ,
            events.as_mut_ptr() as u64,
            events.len() as u64,
            timeout_ms,
        )
    })
    .map(|count| count as usize)
}

/// Reads the whole file at `path`.
pub fn read_file(path: &str) -> Result<Vec<u8>, Errno> {
    let fd = open(path, abi::open::READ)?;
    let mut contents = Vec::new();
    let mut buffer = alloc::vec![0u8; 16 * 1024];
    let outcome = loop {
        match read(fd, &mut buffer) {
            Ok(0) => break Ok(()),
            Ok(count) => contents.extend_from_slice(&buffer[..count]),
            Err(error) => break Err(error),
        }
    };
    let _ = close(fd);
    outcome.map(|()| contents)
}

/// Information about the file or directory at `path`.
pub fn stat(path: &str) -> Result<abi::Stat, Errno> {
    let mut stat = abi::Stat::default();
    // SAFETY: the path is a live string and `stat` a live, writable value
    // of exactly the type the kernel fills in.
    result(unsafe {
        syscall(
            nr::STAT,
            path.as_ptr() as u64,
            path.len() as u64,
            &raw mut stat as u64,
        )
    })?;
    Ok(stat)
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's name.
    pub name: String,
    /// Whether it is a directory.
    pub is_dir: bool,
}

/// Lists the directory at `path`, sorted by name.
pub fn read_dir(path: &str) -> Result<Vec<DirEntry>, Errno> {
    let mut buffer = alloc::vec![0u8; 1024];
    loop {
        // SAFETY: the path is a live string and the buffer a live, writable
        // allocation of the stated length.
        let needed = result(unsafe {
            syscall4(
                nr::READDIR,
                path.as_ptr() as u64,
                path.len() as u64,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
            )
        })? as usize;
        if needed > buffer.len() {
            // Did not fit (or the directory grew): retry with room to spare.
            buffer.resize(needed * 2, 0);
            continue;
        }
        return Ok(buffer[..needed]
            .split_inclusive(|&byte| byte == 0)
            .filter_map(|record| {
                let (&kind, name) = record.split_first()?;
                let name = core::str::from_utf8(name.strip_suffix(&[0])?).ok()?;
                Some(DirEntry {
                    name: name.into(),
                    is_dir: u64::from(kind) == abi::KIND_DIRECTORY,
                })
            })
            .collect());
    }
}

fn path_call(number: u64, path: &str) -> Result<(), Errno> {
    // SAFETY: the pointer and length describe a live, readable string.
    result(unsafe { syscall(number, path.as_ptr() as u64, path.len() as u64, 0) }).map(|_| ())
}

/// Creates the directory `path`.
pub fn mkdir(path: &str) -> Result<(), Errno> {
    path_call(nr::MKDIR, path)
}

/// Removes the file or empty directory `path`.
pub fn remove(path: &str) -> Result<(), Errno> {
    path_call(nr::REMOVE, path)
}

/// Moves or renames `from` to `to`.
pub fn rename(from: &str, to: &str) -> Result<(), Errno> {
    // SAFETY: both pointer/length pairs describe live, readable strings.
    result(unsafe {
        syscall4(
            nr::RENAME,
            from.as_ptr() as u64,
            from.len() as u64,
            to.as_ptr() as u64,
            to.len() as u64,
        )
    })
    .map(|_| ())
}

/// Starts the program at `path` with `arguments` (conventionally starting
/// with the program's name) and returns its process ID.
pub fn spawn(path: &str, arguments: &[&str]) -> Result<u64, Errno> {
    spawn_with_output(path, arguments, None)
}

/// Like [`spawn`]; with `output`, a descriptor open for writing, the new
/// program's standard output is appended to that file instead of going
/// where this program's does.
pub fn spawn_with_output(
    path: &str,
    arguments: &[&str],
    output: Option<u64>,
) -> Result<u64, Errno> {
    spawn_with_stdio(path, arguments, None, output, None)
}

/// Creates a pipe; returns the descriptors of its reading and writing
/// ends. What is written to one can be read from the other, by this
/// program or by programs started with [`spawn_with_stdio`]. Reading
/// returns 0 once the pipe is empty and every writing end is closed — so
/// a program that hands the writing end to another must close its own
/// copy to ever see the end.
pub fn pipe() -> Result<(u64, u64), Errno> {
    let mut ends = [0u64; 2];
    // SAFETY: `ends` is a live, writable array of two `u64`.
    result(unsafe { syscall(nr::PIPE, ends.as_mut_ptr() as u64, 0, 0) })?;
    Ok((ends[0], ends[1]))
}

/// Like [`spawn`], connecting the new program's standard input, output
/// and error streams to the given descriptors (pipe ends or files);
/// `None` gives it this program's own stream.
pub fn spawn_with_stdio(
    path: &str,
    arguments: &[&str],
    stdin: Option<u64>,
    stdout: Option<u64>,
    stderr: Option<u64>,
) -> Result<u64, Errno> {
    let mut block = Vec::new();
    for argument in arguments {
        block.extend_from_slice(argument.as_bytes());
        block.push(0);
    }
    // SAFETY: both pointer/length pairs describe live, readable buffers.
    result(unsafe {
        syscall5(
            nr::SPAWN,
            path.as_ptr() as u64,
            path.len() as u64,
            block.as_ptr() as u64,
            block.len() as u64,
            abi::spawn_stdio(stdin, stdout, stderr),
        )
    })
}

/// Waits for process `pid` to end and returns its exit status.
pub fn wait(pid: u64) -> Result<i32, Errno> {
    // SAFETY: plain integer argument.
    result(unsafe { syscall(nr::WAIT, pid, 0, 0) }).map(|status| status as i32)
}

/// Sleeps until another thread calls [`futex_wake`] on `word`, provided
/// `word` still holds `expected` when the kernel looks; `timeout_ms` of
/// `None` waits without limit. Returns `Err(WouldBlock)` if the value had
/// changed and `Err(TimedOut)` if the time passed. This is the building
/// block of [`sync::Mutex`] and of joining threads.
pub fn futex_wait(
    word: &core::sync::atomic::AtomicU32,
    expected: u32,
    timeout_ms: Option<u64>,
) -> Result<(), Errno> {
    // SAFETY: the address is that of a live atomic the kernel only reads.
    result(unsafe {
        syscall(
            nr::FUTEX_WAIT,
            core::ptr::from_ref(word) as u64,
            u64::from(expected),
            timeout_ms.unwrap_or(abi::FUTEX_NO_TIMEOUT),
        )
    })
    .map(|_| ())
}

/// Wakes up to `count` threads sleeping in [`futex_wait`] on `word`;
/// returns how many.
pub fn futex_wake(word: &core::sync::atomic::AtomicU32, count: usize) -> usize {
    // SAFETY: the address only identifies the waiters.
    let woken = unsafe {
        syscall(
            nr::FUTEX_WAKE,
            core::ptr::from_ref(word) as u64,
            count as u64,
            0,
        )
    };
    usize::try_from(woken).unwrap_or(0)
}

/// The date and time, as microseconds since 1970-01-01 00:00:00 UTC
/// (see [`abi::datetime`]); `None` if the machine has no usable clock.
pub fn realtime_us() -> Option<u64> {
    // SAFETY: no arguments.
    result(unsafe { syscall(nr::REALTIME, 0, 0, 0) }).ok()
}

/// Microseconds since the system started.
pub fn clock_us() -> u64 {
    // SAFETY: no arguments.
    unsafe { syscall(nr::CLOCK, 0, 0, 0) as u64 }
}

/// Checks whether process `pid` has ended, without waiting: its exit
/// status if so, `None` while it is still running.
pub fn try_wait(pid: u64) -> Result<Option<i32>, Errno> {
    // SAFETY: plain integer arguments.
    match result(unsafe { syscall(nr::WAIT, pid, abi::WAIT_NO_HANG, 0) }) {
        Ok(status) => Ok(Some(status as i32)),
        Err(Errno::Busy) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Asks for process `pid` to be terminated.
pub fn kill(pid: u64) -> Result<(), Errno> {
    // SAFETY: plain integer argument.
    result(unsafe { syscall(nr::KILL, pid, 0, 0) }).map(|_| ())
}

/// The calling process's ID.
pub fn getpid() -> u64 {
    // SAFETY: no arguments.
    unsafe { syscall(nr::GETPID, 0, 0, 0) as u64 }
}

/// Gives up the rest of the current time slice.
pub fn yield_now() {
    // SAFETY: no arguments.
    unsafe { syscall(nr::YIELD, 0, 0, 0) };
}

/// Blocks for at least `milliseconds`.
pub fn sleep_ms(milliseconds: u64) {
    // SAFETY: plain integer argument.
    unsafe { syscall(nr::SLEEP, milliseconds, 0, 0) };
}

/// Milliseconds since the system booted.
pub fn uptime_ms() -> u64 {
    // SAFETY: no arguments.
    unsafe { syscall(nr::UPTIME, 0, 0, 0) as u64 }
}

/// A [`fmt::Write`] sink for a file descriptor.
pub struct Fd(pub u64);

impl fmt::Write for Fd {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut bytes = text.as_bytes();
        while !bytes.is_empty() {
            match write(self.0, bytes) {
                Ok(0) | Err(_) => return Err(fmt::Error),
                Ok(written) => bytes = &bytes[written..],
            }
        }
        Ok(())
    }
}

/// Prints to standard output.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use ::core::fmt::Write as _;
        let _ = ::core::write!($crate::Fd($crate::abi::STDOUT), $($arg)*);
    }};
}

/// Prints a line to standard output.
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        use ::core::fmt::Write as _;
        let _ = ::core::writeln!($crate::Fd($crate::abi::STDOUT), $($arg)*);
    }};
}

/// Prints a line to standard error.
#[macro_export]
macro_rules! eprintln {
    ($($arg:tt)*) => {{
        use ::core::fmt::Write as _;
        let _ = ::core::writeln!($crate::Fd($crate::abi::STDERR), $($arg)*);
    }};
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    use core::fmt::Write as _;
    let _ = writeln!(Fd(omit_abi::STDERR), "panic: {info}");
    exit(omit_abi::EXIT_PANIC)
}
