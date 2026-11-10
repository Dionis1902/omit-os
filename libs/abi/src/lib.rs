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
    /// `open(path: *const u8, path_len, flags) -> fd` — see [`super::open`].
    pub const OPEN: u64 = 6;
    /// `read(fd, buf: *mut u8, len) -> bytes read` — 0 means end of file.
    /// Reading descriptor 0 returns one line of console input.
    pub const READ: u64 = 7;
    /// `close(fd)`.
    pub const CLOSE: u64 = 8;
    /// `map(len, align) -> address` — maps `len` bytes (rounded up to whole
    /// pages) of zero-filled, readable and writable memory at an address
    /// that is a multiple of `align` (a power of two, at least one page).
    pub const MAP: u64 = 9;
    /// `unmap(address, len)` — releases memory obtained from `map`.
    pub const UNMAP: u64 = 10;
    /// `stat(path, path_len, out: *mut Stat)`.
    pub const STAT: u64 = 11;
    /// `readdir(path, path_len, buf, buf_len) -> bytes needed` — fills
    /// `buf` with one record per entry: a type byte ([`super::KIND_FILE`]
    /// or [`super::KIND_DIRECTORY`]), the name, and a NUL. If the result is
    /// larger than `buf_len` the listing did not fit and must be retried
    /// with a larger buffer.
    pub const READDIR: u64 = 12;
    /// `mkdir(path, path_len)`.
    pub const MKDIR: u64 = 13;
    /// `remove(path, path_len)` — removes a file or an empty directory.
    pub const REMOVE: u64 = 14;
    /// `rename(from, from_len, to, to_len)`.
    pub const RENAME: u64 = 15;
    /// `spawn(path, path_len, args, args_len) -> pid` — starts the program
    /// at `path`. `args` is the argument list as NUL-terminated strings,
    /// one after another. The child inherits the working directory and
    /// standard output of the caller.
    pub const SPAWN: u64 = 16;
    /// `wait(pid) -> exit status` — blocks until the process ends.
    pub const WAIT: u64 = 17;
}

/// [`Stat::kind`] of a regular file.
pub const KIND_FILE: u64 = 1;
/// [`Stat::kind`] of a directory.
pub const KIND_DIRECTORY: u64 = 2;

/// File information returned by the `stat` system call.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stat {
    /// [`KIND_FILE`] or [`KIND_DIRECTORY`].
    pub kind: u64,
    /// Size in bytes for files; number of entries for directories.
    pub size: u64,
}

/// Lowest address the kernel hands out for `map` requests.
pub const USER_MAP_BASE: u64 = 0x0000_1000_0000_0000;
/// Exclusive upper bound of the `map` area.
pub const USER_MAP_END: u64 = 0x0000_2000_0000_0000;
/// Largest single `map` request, in bytes.
pub const MAP_MAX: u64 = 256 << 20;

/// Flags for the `open` system call.
pub mod open {
    /// Open for reading (the default when [`WRITE`] is absent).
    pub const READ: u64 = 0;
    /// Open for writing instead of reading.
    pub const WRITE: u64 = 1 << 0;
    /// Create the file if it does not exist (with [`WRITE`]).
    pub const CREATE: u64 = 1 << 6;
    /// Discard existing contents (with [`WRITE`]).
    pub const TRUNCATE: u64 = 1 << 9;
    /// Every write goes to the end of the file (with [`WRITE`]).
    pub const APPEND: u64 = 1 << 10;
    /// All flags the kernel understands.
    pub const ALL: u64 = WRITE | CREATE | TRUNCATE | APPEND;
}

/// Standard input file descriptor.
pub const STDIN: u64 = 0;
/// Largest number of files a process may have open, including the three
/// standard descriptors.
pub const MAX_OPEN_FILES: usize = 32;
/// Longest path accepted by a system call, in bytes.
pub const PATH_MAX: u64 = 4096;
/// Largest number of bytes one `read` call returns.
pub const READ_MAX: u64 = 64 * 1024;
/// Largest total size of a program's arguments, in bytes.
pub const ARGS_MAX: usize = 4096;

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
    /// No such file or directory.
    NotFound = 2,
    /// Bad file descriptor.
    BadFileDescriptor = 9,
    /// A pointer argument does not refer to accessible user memory.
    Fault = 14,
    /// The file already exists.
    Exists = 17,
    /// A path component is not a directory.
    NotADirectory = 20,
    /// The operation needs a file but the path names a directory.
    IsADirectory = 21,
    /// An argument is out of range.
    InvalidArgument = 22,
    /// The process has too many files open.
    TooManyOpenFiles = 24,
    /// Out of memory or storage.
    NoSpace = 28,
    /// The directory is not empty.
    NotEmpty = 39,
    /// No such process.
    NoSuchProcess = 3,
    /// The storage device failed, or on-disk data is corrupt.
    Io = 5,
    /// The object is in use.
    Busy = 16,
    /// The operation would cross from one filesystem to another.
    CrossDevice = 18,
    /// The file is not an executable program.
    NotExecutable = 8,
    /// Unknown system call number.
    NoSuchSyscall = 38,
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::NotFound => "No such file or directory",
            Self::BadFileDescriptor => "Bad file descriptor",
            Self::Fault => "Bad address",
            Self::Exists => "File exists",
            Self::NotADirectory => "Not a directory",
            Self::IsADirectory => "Is a directory",
            Self::InvalidArgument => "Invalid argument",
            Self::TooManyOpenFiles => "Too many open files",
            Self::NoSpace => "No space left on device",
            Self::NotEmpty => "Directory not empty",
            Self::NoSuchProcess => "No such process",
            Self::Io => "Input/output error",
            Self::Busy => "Device or resource busy",
            Self::CrossDevice => "Invalid cross-device link",
            Self::NotExecutable => "Exec format error",
            Self::NoSuchSyscall => "Function not implemented",
        })
    }
}

impl Errno {
    /// The value a system call returns for this error.
    pub const fn as_result(self) -> i64 {
        -(self as i64)
    }

    /// Decodes a system call result.
    pub const fn from_result(result: i64) -> Option<Self> {
        match -result {
            2 => Some(Self::NotFound),
            9 => Some(Self::BadFileDescriptor),
            14 => Some(Self::Fault),
            17 => Some(Self::Exists),
            20 => Some(Self::NotADirectory),
            21 => Some(Self::IsADirectory),
            22 => Some(Self::InvalidArgument),
            24 => Some(Self::TooManyOpenFiles),
            28 => Some(Self::NoSpace),
            39 => Some(Self::NotEmpty),
            3 => Some(Self::NoSuchProcess),
            5 => Some(Self::Io),
            16 => Some(Self::Busy),
            18 => Some(Self::CrossDevice),
            8 => Some(Self::NotExecutable),
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
            Errno::NotFound,
            Errno::Exists,
            Errno::NotADirectory,
            Errno::IsADirectory,
            Errno::TooManyOpenFiles,
            Errno::NoSpace,
            Errno::NotEmpty,
            Errno::NoSuchProcess,
            Errno::Io,
            Errno::Busy,
            Errno::CrossDevice,
            Errno::NotExecutable,
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
