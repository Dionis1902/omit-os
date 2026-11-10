// SPDX-License-Identifier: GPL-2.0-only
//! The Omit OS native system call ABI.
//!
//! # Calling convention (x86-64)
//!
//! A system call is made with the `syscall` instruction:
//!
//! | Register | Meaning |
//! | --- | --- |
//! | `rax` | system call number on entry, result on return |
//! | `rdi`, `rsi`, `rdx`, `r10`, `r8` | arguments 0-4 |
//! | `rcx`, `r11` | destroyed (used by the CPU for the return address and flags) |
//!
//! `rbx`, `rbp`, `rsp` and `r12`-`r15` are preserved. All other
//! general-purpose registers are zeroed on return.
//!
//! The result is a signed 64-bit value: non-negative on success, or the
//! negation of an [`Errno`] on failure.
//!
//! The ABI is unstable: numbers and semantics may change until the first
//! stable release.

#![no_std]
#![forbid(unsafe_code)]

/// System call numbers.
pub mod nr {
    /// `exit(code: i32) -> !` — terminates the calling process.
    pub const EXIT: u64 = 0;
    /// `write(fd, buf: *const u8, len) -> bytes written` — only the
    /// standard output (1) and standard error (2) descriptors exist; both
    /// go to the console.
    pub const WRITE: u64 = 1;
    /// `getpid() -> pid`.
    pub const GETPID: u64 = 2;
    /// `yield()` — lets other threads run.
    pub const YIELD: u64 = 3;
    /// `sleep(milliseconds)` — blocks for at least that long.
    pub const SLEEP: u64 = 4;
    /// `uptime() -> milliseconds since boot`.
    pub const UPTIME: u64 = 5;
}

/// Standard output file descriptor.
pub const STDOUT: u64 = 1;
/// Standard error file descriptor.
pub const STDERR: u64 = 2;

/// Largest number of bytes one `write` call accepts.
pub const WRITE_MAX: u64 = 1 << 20;

/// Error codes; the values follow the conventional POSIX numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum Errno {
    /// Bad file descriptor.
    BadFileDescriptor = 9,
    /// A pointer argument does not refer to accessible user memory.
    Fault = 14,
    /// An argument is out of range.
    InvalidArgument = 22,
    /// Unknown system call number.
    NoSuchSyscall = 38,
}

impl Errno {
    /// The value a system call returns for this error.
    pub const fn as_result(self) -> i64 {
        -(self as i64)
    }

    /// Decodes a system call result.
    pub const fn from_result(result: i64) -> Option<Self> {
        match -result {
            9 => Some(Self::BadFileDescriptor),
            14 => Some(Self::Fault),
            22 => Some(Self::InvalidArgument),
            38 => Some(Self::NoSuchSyscall),
            _ => None,
        }
    }
}

/// Exit status reported for a process killed by a CPU exception
/// (128 + 11, the conventional encoding of a segmentation fault).
pub const EXIT_FAULT: i32 = 139;
/// Exit status reported for a process terminated with `kill` (128 + 9).
pub const EXIT_KILLED: i32 = 137;
/// Exit status used by the Rust runtime when a program panics.
pub const EXIT_PANIC: i32 = 101;

/// Lowest address a user program may occupy; the page at 0 is never
/// mapped, so null-pointer dereferences fault.
pub const USER_MIN: u64 = 0x1000;
/// Exclusive upper bound of user address space.
pub const USER_MAX: u64 = 0x0000_7FFF_FFFF_F000;
/// Initial stack pointer of a new process.
pub const USER_STACK_TOP: u64 = USER_MAX;
/// Size of the initial user stack.
pub const USER_STACK_SIZE: u64 = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_round_trip() {
        for errno in [
            Errno::BadFileDescriptor,
            Errno::Fault,
            Errno::InvalidArgument,
            Errno::NoSuchSyscall,
        ] {
            assert!(errno.as_result() < 0);
            assert_eq!(Errno::from_result(errno.as_result()), Some(errno));
        }
        assert_eq!(Errno::from_result(0), None);
        assert_eq!(Errno::from_result(5), None);
        assert_eq!(Errno::from_result(-1), None);
    }

    #[test]
    fn user_layout_is_sane() {
        assert!(USER_MIN < USER_MAX);
        assert_eq!(USER_STACK_TOP % 16, 0);
        assert_eq!(USER_STACK_SIZE % 4096, 0);
        // Canonical lower-half addresses only.
        assert!(USER_MAX <= 0x0000_8000_0000_0000);
    }
}
