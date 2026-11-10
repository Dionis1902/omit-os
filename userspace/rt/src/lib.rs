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
//! panic and exits with status 101. There is no heap, no arguments and no
//! environment yet.

#![no_std]

use core::arch::{asm, global_asm};
use core::fmt;

pub use omit_abi as abi;
use omit_abi::{Errno, nr};

// The kernel starts a process with RSP at the top of its stack and nothing
// on it. Align the stack as the ABI expects at a call site and enter Rust.
global_asm!(
    r#"
.section .text._start, "ax", @progbits
.global _start
_start:
    xor ebp, ebp
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

extern "C" fn rust_start() -> ! {
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
            lateout("rcx") _, lateout("r8") _, lateout("r9") _,
            lateout("r10") _, lateout("r11") _,
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
    let result = unsafe { syscall(nr::WRITE, fd, bytes.as_ptr() as u64, bytes.len() as u64) };
    usize::try_from(result)
        .map_err(|_| Errno::from_result(result).unwrap_or(Errno::InvalidArgument))
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

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    use core::fmt::Write as _;
    let _ = writeln!(Fd(omit_abi::STDERR), "panic: {info}");
    exit(omit_abi::EXIT_PANIC)
}
