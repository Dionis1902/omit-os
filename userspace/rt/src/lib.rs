// SPDX-License-Identifier: GPL-2.0-only
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

#![no_std]

extern crate alloc;

mod heap;

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
            lateout("rcx") _, lateout("r8") _, lateout("r9") _,
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
    let mut block = Vec::new();
    for argument in arguments {
        block.extend_from_slice(argument.as_bytes());
        block.push(0);
    }
    // SAFETY: both pointer/length pairs describe live, readable buffers.
    result(unsafe {
        syscall4(
            nr::SPAWN,
            path.as_ptr() as u64,
            path.len() as u64,
            block.as_ptr() as u64,
            block.len() as u64,
        )
    })
}

/// Waits for process `pid` to end and returns its exit status.
pub fn wait(pid: u64) -> Result<i32, Errno> {
    // SAFETY: plain integer argument.
    result(unsafe { syscall(nr::WAIT, pid, 0, 0) }).map(|status| status as i32)
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
