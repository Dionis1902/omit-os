// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only WITH LicenseRef-OmitOS-syscall-note
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

pub mod datetime;

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
    /// `spawn(path, path_len, args, args_len, stdio) -> pid` — starts the
    /// program at `path`. `args` is the argument list as NUL-terminated
    /// strings, one after another. The child inherits the working
    /// directory of the caller, and its standard input, output and error
    /// streams unless `stdio` (see [`super::spawn_stdio`]) says otherwise.
    pub const SPAWN: u64 = 16;
    /// `wait(pid, flags) -> exit status` — blocks until the process ends.
    /// With [`super::WAIT_NO_HANG`] in `flags` it fails with `Busy`
    /// instead of blocking while the process is still running.
    pub const WAIT: u64 = 17;
    /// `protect(address, len, protection)` — changes the access rights of
    /// memory obtained from `map`; see [`super::protect`]. Memory can never
    /// be writable and executable at once.
    pub const PROTECT: u64 = 18;
    /// `set_thread_pointer(address)` — sets the base of the `GS` segment
    /// for the calling thread, for runtimes that keep per-thread data
    /// there (such as the Windows TEB).
    pub const SET_THREAD_POINTER: u64 = 19;
    /// `display_info(info)` — fills in a [`super::DisplayInfo`]; fails
    /// with `NotFound` if the machine has no screen.
    pub const DISPLAY_INFO: u64 = 20;
    /// `display_acquire()` — makes the calling process the owner of the
    /// screen and of keyboard and mouse input. The text console stops
    /// drawing until the owner releases the display or exits. Fails with
    /// `Busy` if another process owns it.
    pub const DISPLAY_ACQUIRE: u64 = 21;
    /// `display_release()` — gives the screen back to the text console.
    pub const DISPLAY_RELEASE: u64 = 22;
    /// `display_blit(request)` — copies a rectangle of pixels to the
    /// screen; see [`super::Blit`]. Only the display owner may call it.
    pub const DISPLAY_BLIT: u64 = 23;
    /// `input_read(events, capacity, timeout_ms) -> count` — waits up to
    /// `timeout_ms` for keyboard and mouse events and stores up to
    /// `capacity` [`super::InputEvent`]s. Only the display owner may call
    /// it.
    pub const INPUT_READ: u64 = 24;
    /// `kill(pid)` — asks for process `pid` to be terminated. It ends the
    /// next time it would return to user mode.
    pub const KILL: u64 = 25;
    /// `clock() -> microseconds` — time since the system started, with
    /// microsecond resolution.
    pub const CLOCK: u64 = 26;
    /// `pipe(ends)` — creates a pipe and stores two descriptors in the
    /// array of two `u64` at `ends`: the reading end, then the writing
    /// end. Reading returns 0 once the pipe is empty and every writing
    /// end is closed; writing fails with `BrokenPipe` once every reading
    /// end is closed.
    pub const PIPE: u64 = 27;
    /// `thread_create(entry, stack, argument, exit_flag) -> thread id` —
    /// starts another thread in the calling process. It begins executing
    /// at `entry` with `argument` in RDI and `stack` as its stack pointer
    /// (which must be 8 below a multiple of 16, as at a function's first
    /// instruction). When the thread has ended, the kernel stores 1 in
    /// the `u32` at `exit_flag` (if that is not zero) and wakes futex
    /// waiters on it; the thread does not touch its stack after that.
    pub const THREAD_CREATE: u64 = 28;
    /// `thread_exit()` — ends the calling thread. The process ends (with
    /// status 0) when its last thread does; `exit` ends all threads.
    pub const THREAD_EXIT: u64 = 29;
    /// `futex_wait(address, expected, timeout_ms)` — if the `u32` at
    /// `address` equals `expected`, sleeps until another thread calls
    /// `futex_wake` on the address or the timeout passes
    /// ([`super::FUTEX_NO_TIMEOUT`] for none). Returns 0 when woken, and
    /// fails with `WouldBlock` if the value differed or `TimedOut` if the
    /// time passed.
    pub const FUTEX_WAIT: u64 = 30;
    /// `futex_wake(address, count) -> woken` — wakes up to `count`
    /// threads of the calling process sleeping in `futex_wait` on
    /// `address`.
    pub const FUTEX_WAKE: u64 = 31;
    /// `shared_create(length, token_out) -> address` — creates a shared
    /// memory segment of `length` bytes (rounded up to pages, at most
    /// [`super::SHARED_MAX`]), zero-filled, maps it writable into the
    /// caller and stores the segment's token (a `u64`) at `token_out`.
    /// Any process that knows the token can map the same memory. The
    /// segment exists while it is mapped somewhere.
    pub const SHARED_CREATE: u64 = 32;
    /// `shared_map(token, length_out) -> address` — maps the segment
    /// named by `token` writable into the caller and stores its length in
    /// bytes at `length_out`. Fails with `NotFound` if no such segment
    /// exists (any more). `unmap` with the segment's address and length
    /// removes the mapping.
    pub const SHARED_MAP: u64 = 33;
    /// `channel_listen(name, name_len) -> fd` — claims the name and
    /// returns a descriptor on which connections to it arrive. Fails with
    /// `Exists` while another listener has the name.
    pub const CHANNEL_LISTEN: u64 = 34;
    /// `channel_accept(fd, flags) -> fd` — takes the next connection made
    /// to the listener: a descriptor that can be read and written, each
    /// direction a byte stream. Waits for one unless `flags` has
    /// [`super::READ_NO_WAIT`], in which case it fails with `WouldBlock`.
    pub const CHANNEL_ACCEPT: u64 = 35;
    /// `channel_connect(name, name_len) -> fd` — connects to the listener
    /// of that name. Fails with `NotFound` if there is none.
    pub const CHANNEL_CONNECT: u64 = 36;
    /// `poll(fds, count, timeout_ms, flags) -> ready` — waits until one
    /// of the `count` (at most [`super::POLL_MAX`]) descriptors in the
    /// `u64` array `fds` can be read without waiting (data, end of
    /// input, or a connection to accept), or the time has passed. Returns
    /// a bit mask: bit *i* is set if `fds[i]` is ready. With
    /// [`super::POLL_INPUT`] in `flags`, the call also ends when an input
    /// event is pending for the display owner, reported as
    /// [`super::POLL_INPUT_READY`]. 0 means the time passed.
    pub const POLL: u64 = 37;
    /// `realtime() -> microseconds` — the date and time, as microseconds
    /// since 1970-01-01 00:00:00 UTC (see [`super::datetime`]). Fails
    /// with `Io` if the machine's clock could not be read at start-up.
    pub const REALTIME: u64 = 38;
    /// `seek(fd, offset: i64, whence) -> position` — moves the position
    /// of an open file to `offset` bytes from the place `whence` names
    /// (see [`super::seek`]) and returns the new position from the start
    /// of the file. The position may lie beyond the end: reading there
    /// returns nothing, writing there extends the file with zeros.
    /// Fails with `InvalidArgument` for a position before the start and
    /// `BadFileDescriptor` for anything but a file.
    pub const SEEK: u64 = 39;
    /// `udp_bind(port) -> fd`: opens a UDP socket on local port `port`,
    /// or on a free port chosen by the kernel if `port` is 0. Fails with
    /// `Busy` if the port is taken.
    pub const UDP_BIND: u64 = 40;
    /// `udp_send(fd, to, data, length) -> length`: sends one datagram of
    /// at most [`crate::net::UDP_PAYLOAD_MAX`] bytes to the address and
    /// port in the [`crate::net::SocketAddr`] at `to`. May wait up to a
    /// second for the next hop's hardware address to be found; fails
    /// with `Unreachable` if it is not, or if there is no network.
    pub const UDP_SEND: u64 = 41;
    /// `udp_receive(fd, buffer, length, from, timeout_ms) -> length`:
    /// takes the next datagram received on the socket, waiting up to
    /// `timeout_ms` milliseconds ([`crate::net::NO_TIMEOUT`]: for ever;
    /// 0: not at all) and failing with `TimedOut` (or `WouldBlock` for
    /// 0) if none arrives. A datagram longer than the buffer is cut
    /// short. If `from` is not 0 the sender is written there as a
    /// [`crate::net::SocketAddr`].
    pub const UDP_RECEIVE: u64 = 42;
    /// `net_info(out) -> 0`: writes a [`crate::net::NetInfo`] describing
    /// the network interface.
    pub const NET_INFO: u64 = 43;
    /// `tcp_connect(to, timeout_ms) -> fd`: opens a TCP connection to
    /// the address and port in the [`crate::net::SocketAddr`] at `to`,
    /// waiting up to `timeout_ms` milliseconds for the other side to
    /// answer ([`crate::net::NO_TIMEOUT`]: until the system itself gives
    /// up, about a minute). Fails with `Refused` if nothing listens
    /// there, `Unreachable` if there is no way to the address, and
    /// `TimedOut`. The descriptor is read with `read` (0: the other
    /// side has finished sending), written with `write`, usable with
    /// `poll`, and closed with `close`, after which what was written is
    /// still delivered.
    pub const TCP_CONNECT: u64 = 44;
    /// `tcp_listen(port) -> fd`: starts accepting TCP connections on
    /// `port`. Fails with `Busy` if somebody already does. `poll`
    /// reports the descriptor readable when a connection is waiting.
    pub const TCP_LISTEN: u64 = 45;
    /// `tcp_accept(fd, from, timeout_ms) -> fd`: takes the next
    /// connection made to the listener `fd`, waiting as `udp_receive`
    /// does. If `from` is not 0 the other side's address and port are
    /// written there.
    pub const TCP_ACCEPT: u64 = 46;
    /// `tcp_shutdown(fd) -> 0`: tells the other side that nothing more
    /// will be sent on the connection `fd`. Reading goes on.
    pub const TCP_SHUTDOWN: u64 = 47;
    /// `system_info(out) -> 0`: writes a [`crate::system::Info`]: memory,
    /// uptime and how much of it the processor was idle.
    pub const SYSTEM_INFO: u64 = 48;
    /// `task_list(out, capacity) -> count`: writes one
    /// [`crate::system::Task`] per thread in the system, ordered by
    /// thread ID, to the array of `capacity` entries (at most
    /// [`crate::system::TASKS_MAX`]) at `out`, and returns how many
    /// threads there are — which can be more than were written.
    pub const TASK_LIST: u64 = 49;
    /// `random(out, length) -> quality`: fills the `length` bytes (at
    /// most [`crate::RANDOM_MAX`]) at `out` with random bytes for
    /// cryptographic use. Returns [`crate::RANDOM_HARDWARE`] if the
    /// processor's random number generator went into them, and
    /// [`crate::RANDOM_TIMING`] if they rest only on the timing of
    /// events in this machine — which may be guessable, above all in a
    /// virtual machine; a program doing something that matters should
    /// then say so.
    pub const RANDOM: u64 = 50;
}

/// Most bytes one `random` call gives.
pub const RANDOM_MAX: u64 = 4096;
/// `random`: the processor's random number generator was used.
pub const RANDOM_HARDWARE: u64 = 0;
/// `random`: only the timing of events was available as a source.
pub const RANDOM_TIMING: u64 = 1;

/// Largest shared memory segment, in bytes.
pub const SHARED_MAX: u64 = 64 << 20;
/// Longest channel name, in bytes.
pub const CHANNEL_NAME_MAX: u64 = 64;
/// Most descriptors one `poll` call examines.
pub const POLL_MAX: u64 = 32;
/// Longest `poll` timeout, in milliseconds.
pub const POLL_TIMEOUT_MAX: u64 = 60_000;
/// `poll` flag: also wake for input events (display owner only).
pub const POLL_INPUT: u64 = 1;
/// Bit in the result of `poll`: an input event is pending. (Not the top
/// bit: results with it set are error codes.)
pub const POLL_INPUT_READY: u64 = 1 << 62;
/// Flag for the `write` system call (fourth argument): if the descriptor
/// is a pipe or channel without room for all the data, fail with
/// `WouldBlock` and write nothing, instead of waiting.
pub const WRITE_NO_WAIT: u64 = 1;

/// `futex_wait` timeout value meaning "wait without limit".
pub const FUTEX_NO_TIMEOUT: u64 = u64::MAX;

/// Packs the standard streams for the `spawn` system call's fifth
/// argument. Each is `None` to give the new program the caller's own
/// stream, or a descriptor the caller has open: a pipe end, or a file
/// (for output: one open for writing, to which the output is appended;
/// for input: one open for reading, read from its start).
pub const fn spawn_stdio(stdin: Option<u64>, stdout: Option<u64>, stderr: Option<u64>) -> u64 {
    const fn field(descriptor: Option<u64>) -> u64 {
        match descriptor {
            // Descriptors are small numbers; 0 means "inherit".
            Some(fd) => (fd + 1) & 0xFFFF,
            None => 0,
        }
    }
    field(stdout) | (field(stdin) << 16) | (field(stderr) << 32)
}

// (A file is opened for reading, for writing, or — with
// `open::READ_WRITE` — for both.)

/// Flag for the `read` system call (fourth argument): if the descriptor
/// is a pipe with nothing in it, fail with `WouldBlock` instead of
/// waiting. Files never make a reader wait; the console cannot be read
/// this way.
pub const READ_NO_WAIT: u64 = 1;

/// Flag for the `wait` system call: do not block.
pub const WAIT_NO_HANG: u64 = 1;

/// Size of the screen.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DisplayInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Argument of the `display_blit` system call: copy `width` x `height`
/// pixels to the screen with their top-left corner at (`x`, `y`). The
/// rectangle must lie on the screen.
///
/// A pixel is a `u32` holding `0x00RRGGBB`. The source is read from
/// `pixels`, `stride` pixels per row (at least `width`), which lets a
/// program update part of the screen from a full-screen buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Blit {
    /// Address of the first source pixel.
    pub pixels: u64,
    /// Source pixels per row.
    pub stride: u32,
    /// Destination column.
    pub x: u32,
    /// Destination row.
    pub y: u32,
    /// Width of the rectangle.
    pub width: u32,
    /// Height of the rectangle.
    pub height: u32,
}

/// One keyboard or mouse event, as returned by `input_read`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputEvent {
    /// [`event::KEY`] or [`event::MOUSE`].
    pub kind: u32,
    /// Key: the key code (see [`key`]). Mouse: movement to the right.
    /// Wheel: notches turned, positive towards the person (which
    /// scrolls down).
    pub a: i32,
    /// Mouse: movement down the screen.
    pub b: i32,
    /// Mouse: the buttons held (bit 0 left, bit 1 right, bit 2 middle).
    /// Key and modifiers: the modifier keys held (see [`modifier`]).
    pub c: u32,
    /// When the kernel received the event from the device, in
    /// microseconds on the clock of the `clock` system call. For mouse
    /// movement merged from several reports, the time of the first.
    pub time_us: u64,
}

/// Event kinds.
pub mod event {
    /// A key was pressed.
    pub const KEY: u32 = 1;
    /// The mouse moved or a button changed.
    pub const MOUSE: u32 = 2;
    /// A modifier key (Shift, Ctrl, Alt) was pressed or let go; the
    /// event says which are held now.
    pub const MODIFIERS: u32 = 3;
    /// The mouse's wheel turned.
    pub const WHEEL: u32 = 4;
}

/// The modifier keys, as bits of [`InputEvent::c`] in key and modifier
/// events.
pub mod modifier {
    /// Either Shift key.
    pub const SHIFT: u32 = 1 << 0;
    /// Either Ctrl key.
    pub const CONTROL: u32 = 1 << 1;
    /// Either Alt key.
    pub const ALT: u32 = 1 << 2;
}

/// Key codes. Keys with an ASCII meaning have that value (Enter is 13,
/// Backspace 127, Escape 27, Ctrl+letter 1-26); the others follow.
pub mod key {
    /// Cursor up.
    pub const UP: i32 = 0x101;
    /// Cursor down.
    pub const DOWN: i32 = 0x102;
    /// Cursor left.
    pub const LEFT: i32 = 0x103;
    /// Cursor right.
    pub const RIGHT: i32 = 0x104;
    /// Home.
    pub const HOME: i32 = 0x105;
    /// End.
    pub const END: i32 = 0x106;
    /// Page Up.
    pub const PAGE_UP: i32 = 0x107;
    /// Page Down.
    pub const PAGE_DOWN: i32 = 0x108;
    /// Insert.
    pub const INSERT: i32 = 0x109;
    /// Delete.
    pub const DELETE: i32 = 0x10A;
    /// F1; F2 to F12 follow consecutively.
    pub const F1: i32 = 0x111;
}

/// Most events one `input_read` call returns.
pub const INPUT_READ_MAX: u64 = 64;
/// Longest `input_read` timeout, in milliseconds.
pub const INPUT_TIMEOUT_MAX: u64 = 60_000;

/// Protection flags for the `protect` system call. Memory is always
/// readable.
pub mod protect {
    /// Read-only.
    pub const READ: u64 = 0;
    /// Readable and writable.
    pub const WRITE: u64 = 1 << 0;
    /// Readable and executable.
    pub const EXECUTE: u64 = 1 << 1;
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
    /// Open for reading and writing. [`CREATE`], [`TRUNCATE`] and
    /// [`APPEND`] may be combined with it as with [`WRITE`].
    pub const READ_WRITE: u64 = 1 << 1;
    /// All flags the kernel understands.
    pub const ALL: u64 = WRITE | READ_WRITE | CREATE | TRUNCATE | APPEND;
}

/// What the offset given to the `seek` system call is relative to.
pub mod seek {
    /// The start of the file.
    pub const START: u64 = 0;
    /// The current position.
    pub const CURRENT: u64 = 1;
    /// The end of the file.
    pub const END: u64 = 2;
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
    /// Writing to a pipe that nobody reads from any more.
    BrokenPipe = 32,
    /// Nothing is available right now, and the caller asked not to wait.
    WouldBlock = 11,
    /// The time allowed for the operation has passed.
    TimedOut = 110,
    /// The destination cannot be reached: no network, no route, or
    /// nobody answers for the address.
    Unreachable = 101,
    /// Nothing listens on the port a connection was attempted to.
    Refused = 111,
    /// The other side of a connection abandoned it.
    ConnectionReset = 104,
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
            Self::BrokenPipe => "Broken pipe",
            Self::WouldBlock => "Resource temporarily unavailable",
            Self::TimedOut => "Timed out",
            Self::Unreachable => "Network is unreachable",
            Self::Refused => "Connection refused",
            Self::ConnectionReset => "Connection reset by peer",
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
        // (`wrapping_neg`: the most negative value is no error code, and
        // must not overflow either.)
        match result.wrapping_neg() {
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
            32 => Some(Self::BrokenPipe),
            11 => Some(Self::WouldBlock),
            110 => Some(Self::TimedOut),
            101 => Some(Self::Unreachable),
            111 => Some(Self::Refused),
            104 => Some(Self::ConnectionReset),
            _ => None,
        }
    }
}

/// What the system reports about itself: `system_info` and `task_list`.
pub mod system {
    /// Most entries one `task_list` call writes.
    pub const TASKS_MAX: u64 = 256;
    /// Bytes of a thread's name that [`Task`] has room for.
    pub const NAME_MAX: usize = 32;

    /// [`Task::state`]: executing at the moment of the call.
    pub const STATE_RUNNING: u32 = 0;
    /// [`Task::state`]: waiting for its turn on the processor.
    pub const STATE_READY: u32 = 1;
    /// [`Task::state`]: waiting for an event or a timeout.
    pub const STATE_BLOCKED: u32 = 2;
    /// [`Task::state`]: finished, not yet cleared away.
    pub const STATE_DEAD: u32 = 3;

    /// [`Task::flags`]: the thread that runs when no other can; its
    /// processor time is the time the processor was idle.
    pub const TASK_IDLE: u32 = 1 << 0;

    /// The system as a whole, as `system_info` describes it.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Info {
        /// Bytes of physical memory the kernel manages.
        pub memory_total: u64,
        /// Bytes of it that are not in use.
        pub memory_free: u64,
        /// Bytes the kernel's own heap has handed out.
        pub kernel_heap: u64,
        /// Milliseconds since the system started.
        pub uptime_ms: u64,
        /// Milliseconds of them in which the processor had nothing to
        /// run.
        pub idle_ms: u64,
        /// Threads in the system.
        pub threads: u32,
        /// Processes among them.
        pub processes: u32,
    }

    /// One thread, as `task_list` describes it.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Task {
        /// The thread's ID.
        pub thread: u64,
        /// The ID of the process it belongs to; 0 for a thread of the
        /// kernel itself.
        pub pid: u64,
        /// Milliseconds of processor time it has used.
        pub cpu_ms: u64,
        /// One of the `STATE_*` values.
        pub state: u32,
        /// How many bytes of `name` are used.
        pub name_len: u32,
        /// `TASK_*` bits.
        pub flags: u32,
        /// Zero.
        pub reserved: u32,
        /// The thread's name (for a process, its program), cut off at
        /// [`NAME_MAX`] bytes; UTF-8 except possibly for a character
        /// cut in the middle.
        pub name: [u8; NAME_MAX],
    }

    impl Default for Task {
        fn default() -> Self {
            Self {
                thread: 0,
                pid: 0,
                cpu_ms: 0,
                state: STATE_DEAD,
                name_len: 0,
                flags: 0,
                reserved: 0,
                name: [0; NAME_MAX],
            }
        }
    }

    const _: () = assert!(size_of::<Info>() == 48);
    const _: () = assert!(size_of::<Task>() == 72);
}

/// Networking structures and limits.
pub mod net {
    /// Longest UDP datagram payload: what fits one Ethernet frame.
    pub const UDP_PAYLOAD_MAX: u64 = 1472;
    /// `timeout_ms` value of `udp_receive` that means "wait for ever".
    pub const NO_TIMEOUT: u64 = u64::MAX;
    /// Longest wait `udp_receive` accepts otherwise, in milliseconds.
    pub const RECEIVE_TIMEOUT_MAX: u64 = 3_600_000;

    /// An IPv4 address and a UDP port.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct SocketAddr {
        /// The address, most significant octet first.
        pub address: [u8; 4],
        /// The port.
        pub port: u16,
        /// Zero.
        pub reserved: u16,
    }

    /// [`NetInfo::flags`]: the machine has a network interface.
    pub const INFO_PRESENT: u32 = 1 << 0;
    /// [`NetInfo::flags`]: the interface has an address.
    pub const INFO_CONFIGURED: u32 = 1 << 1;
    /// [`NetInfo::flags`]: a router is known.
    pub const INFO_ROUTER: u32 = 1 << 2;
    /// [`NetInfo::flags`]: a name server is known.
    pub const INFO_NAME_SERVER: u32 = 1 << 3;

    /// The network interface, as `net_info` describes it. Fields whose
    /// flag is not set are zero.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct NetInfo {
        /// `INFO_*` bits.
        pub flags: u32,
        /// The interface's address.
        pub address: [u8; 4],
        /// The network mask.
        pub mask: [u8; 4],
        /// The router for other networks.
        pub router: [u8; 4],
        /// The name server.
        pub name_server: [u8; 4],
        /// The hardware address.
        pub mac: [u8; 6],
        /// Zero.
        pub reserved: [u8; 2],
    }

    const _: () = assert!(size_of::<SocketAddr>() == 8);
    const _: () = assert!(size_of::<NetInfo>() == 28);
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
        const { assert!(USER_MIN < USER_MAX) };
        assert_eq!(USER_STACK_TOP % 16, 0);
        assert_eq!(USER_STACK_SIZE % 4096, 0);
        // Canonical lower-half addresses only.
        const { assert!(USER_MAX <= 0x0000_8000_0000_0000) };
    }
}
