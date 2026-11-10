// SPDX-License-Identifier: GPL-2.0-only
//! Support code shared by the Windows test programs.
//!
//! These are real Windows programs: PE32+ console executables for the
//! `x86_64-pc-windows-msvc` target that use nothing but functions
//! imported from `KERNEL32.dll`. They do not link the C runtime or the
//! Rust standard library, which keeps them small and makes their imports
//! exactly the functions each test names. `cargo xtask` builds them with
//! the LLVM linker shipped with Rust; no Windows SDK is needed.
//!
//! Without a C runtime the program has to bring a few things itself: the
//! entry point `mainCRTStartup` (each program defines it), the memory
//! functions the compiler may call, and a panic handler. They are here.

#![no_std]
// The memory functions below must not be compiled into calls to
// themselves.
#![no_builtins]
#![allow(
    missing_docs,
    reason = "the constants and functions are the Windows API's own, documented by Microsoft"
)]

use core::arch::asm;
use core::ffi::{c_char, c_void};
use core::fmt;

/// A Windows `HANDLE`.
pub type Handle = *mut c_void;
/// A Windows `BOOL`.
pub type Bool = i32;

pub const INVALID_HANDLE_VALUE: Handle = usize::MAX as Handle;
pub const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
pub const STD_ERROR_HANDLE: u32 = -12i32 as u32;
pub const GENERIC_READ: u32 = 0x8000_0000;
pub const GENERIC_WRITE: u32 = 0x4000_0000;
pub const CREATE_NEW: u32 = 1;
pub const CREATE_ALWAYS: u32 = 2;
pub const OPEN_EXISTING: u32 = 3;
pub const HEAP_ZERO_MEMORY: u32 = 0x8;
pub const MEM_COMMIT: u32 = 0x1000;
pub const MEM_RESERVE: u32 = 0x2000;
pub const MEM_RELEASE: u32 = 0x8000;
pub const PAGE_READWRITE: u32 = 0x04;
pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    pub safe fn GetStdHandle(which: u32) -> Handle;
    pub fn WriteFile(
        handle: Handle,
        buffer: *const u8,
        length: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> Bool;
    pub fn ReadFile(
        handle: Handle,
        buffer: *mut u8,
        length: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> Bool;
    pub fn WriteConsoleW(
        handle: Handle,
        buffer: *const u16,
        length: u32,
        written: *mut u32,
        reserved: *mut c_void,
    ) -> Bool;
    pub fn CreateFileA(
        name: *const c_char,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    pub safe fn CloseHandle(handle: Handle) -> Bool;
    pub fn GetFileSizeEx(handle: Handle, size: *mut i64) -> Bool;
    pub fn DeleteFileA(name: *const c_char) -> Bool;
    pub fn CreateDirectoryA(name: *const c_char, security: *mut c_void) -> Bool;
    pub fn RemoveDirectoryA(name: *const c_char) -> Bool;
    pub fn MoveFileA(from: *const c_char, to: *const c_char) -> Bool;
    pub fn GetFileAttributesA(name: *const c_char) -> u32;
    pub safe fn GetLastError() -> u32;
    pub safe fn SetLastError(code: u32);
    pub safe fn ExitProcess(code: u32) -> !;
    pub safe fn GetCommandLineA() -> *const c_char;
    pub safe fn GetCurrentProcessId() -> u32;
    pub fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
    pub safe fn GetProcessHeap() -> Handle;
    pub safe fn HeapAlloc(heap: Handle, flags: u32, size: usize) -> *mut u8;
    pub fn HeapReAlloc(heap: Handle, flags: u32, pointer: *mut u8, size: usize) -> *mut u8;
    pub fn HeapSize(heap: Handle, flags: u32, pointer: *const u8) -> usize;
    pub fn HeapFree(heap: Handle, flags: u32, pointer: *mut u8) -> Bool;
    pub safe fn VirtualAlloc(
        address: *mut c_void,
        size: usize,
        kind: u32,
        protection: u32,
    ) -> *mut c_void;
    pub fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> Bool;
    pub safe fn Sleep(milliseconds: u32);
    pub safe fn GetTickCount64() -> u64;
}

/// Writes to a standard handle; implements [`fmt::Write`] so that the
/// `write!` macros work.
pub struct Output(pub u32);

impl fmt::Write for Output {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut written = 0;
        // SAFETY: the buffer is a live string of the given length and
        // `written` a live variable.
        let ok = unsafe {
            WriteFile(
                GetStdHandle(self.0),
                text.as_ptr(),
                text.len() as u32,
                &raw mut written,
                core::ptr::null_mut(),
            )
        };
        if ok != 0 && written as usize == text.len() {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

/// Prints a line to standard output. Lines end with `\r\n`, as Windows
/// programs write them.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let mut out = $crate::Output($crate::STD_OUTPUT_HANDLE);
        let _ = write!(out, $($arg)*);
        let _ = out.write_str("\r\n");
    }};
}

/// The program's command line.
pub fn command_line() -> &'static str {
    // SAFETY: `GetCommandLineA` returns a NUL-terminated string that
    // stays valid and unchanged for the life of the process.
    let line = unsafe { core::ffi::CStr::from_ptr(GetCommandLineA()) };
    line.to_str().unwrap_or("")
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    use fmt::Write as _;
    let mut out = Output(STD_ERROR_HANDLE);
    let _ = write!(out, "panic: {info}\r\n");
    ExitProcess(3)
}

/// The MSVC toolchain's marker that a program uses floating point; the
/// linker looks for it.
#[unsafe(no_mangle)]
pub static _fltused: i32 = 0;

/// `memcpy`, called by compiler-generated code.
///
/// # Safety
///
/// The usual contract: `count` readable bytes at `source`, `count`
/// writable bytes at `destination`, not overlapping.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(destination: *mut u8, source: *const u8, count: usize) -> *mut u8 {
    // SAFETY: `rep movsb` copies RCX bytes from [RSI] to [RDI], which is
    // exactly the caller's contract; the direction flag is clear by the
    // calling convention.
    unsafe {
        asm!(
            "rep movsb",
            inout("rcx") count => _,
            inout("rdi") destination => _,
            inout("rsi") source => _,
            options(nostack, preserves_flags),
        );
    }
    destination
}

/// `memmove`.
///
/// # Safety
///
/// As [`memcpy`], but the ranges may overlap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(destination: *mut u8, source: *const u8, count: usize) -> *mut u8 {
    if (destination as usize).wrapping_sub(source as usize) >= count {
        // The destination does not start inside the source: a forward
        // copy never overwrites bytes it still has to read.
        // SAFETY: forwarded contract.
        return unsafe { memcpy(destination, source, count) };
    }
    // SAFETY: copies backwards, from the last byte, with the direction
    // flag set for the duration and cleared again as the calling
    // convention requires.
    unsafe {
        asm!(
            "std",
            "rep movsb",
            "cld",
            inout("rcx") count => _,
            inout("rdi") destination.add(count).wrapping_sub(1) => _,
            inout("rsi") source.add(count).wrapping_sub(1) => _,
            options(nostack),
        );
    }
    destination
}

/// `memset`.
///
/// # Safety
///
/// `count` writable bytes at `destination`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(destination: *mut u8, value: i32, count: usize) -> *mut u8 {
    // SAFETY: `rep stosb` stores AL into RCX bytes at [RDI].
    unsafe {
        asm!(
            "rep stosb",
            inout("rcx") count => _,
            inout("rdi") destination => _,
            in("al") value as u8,
            options(nostack, preserves_flags),
        );
    }
    destination
}

/// `memcmp`.
///
/// # Safety
///
/// `count` readable bytes at both pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(left: *const u8, right: *const u8, count: usize) -> i32 {
    for index in 0..count {
        // SAFETY: `index < count`, within both ranges.
        let (a, b) = unsafe { (*left.add(index), *right.add(index)) };
        if a != b {
            return i32::from(a) - i32::from(b);
        }
    }
    0
}

/// `strlen`, called by `CStr::from_ptr`.
///
/// # Safety
///
/// `text` must be NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(text: *const c_char) -> usize {
    let mut length = 0;
    // SAFETY: every byte up to the terminator is readable.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    length
}

/// The exception handler that unwinding tables in the precompiled `core`
/// library refer to. These programs are built with `panic=abort` and
/// nothing ever raises an exception, so it cannot be called; if it is
/// anyway, the program ends.
#[unsafe(no_mangle)]
pub extern "C" fn __CxxFrameHandler3() -> ! {
    ExitProcess(0xC000_0409)
}
