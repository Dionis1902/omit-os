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
pub const INFINITE: u32 = u32::MAX;
pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_TIMEOUT: u32 = 258;
pub const STILL_ACTIVE: u32 = 259;

/// `CRITICAL_SECTION`: 40 bytes that belong to the system once
/// initialised.
#[repr(C, align(8))]
pub struct CriticalSection(pub [u8; 40]);

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
    pub fn GetModuleFileNameA(module: *mut c_void, buffer: *mut u8, size: u32) -> u32;
    pub fn LoadLibraryA(name: *const c_char) -> *mut c_void;
    pub safe fn FreeLibrary(module: *mut c_void) -> Bool;
    pub fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
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
    pub fn CreateThread(
        security: *mut c_void,
        stack_size: usize,
        start: unsafe extern "system" fn(*mut c_void) -> u32,
        parameter: *mut c_void,
        flags: u32,
        id: *mut u32,
    ) -> Handle;
    pub safe fn ExitThread(code: u32) -> !;
    pub fn GetExitCodeThread(thread: Handle, code: *mut u32) -> Bool;
    pub safe fn GetCurrentThreadId() -> u32;
    pub safe fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    pub fn CreateEventA(
        security: *mut c_void,
        manual_reset: Bool,
        initial_state: Bool,
        name: *const c_char,
    ) -> Handle;
    pub safe fn SetEvent(event: Handle) -> Bool;
    pub safe fn ResetEvent(event: Handle) -> Bool;
    pub fn InitializeCriticalSection(section: *mut CriticalSection);
    pub fn EnterCriticalSection(section: *mut CriticalSection);
    pub fn TryEnterCriticalSection(section: *mut CriticalSection) -> Bool;
    pub fn LeaveCriticalSection(section: *mut CriticalSection);
    pub fn DeleteCriticalSection(section: *mut CriticalSection);
    pub safe fn TlsAlloc() -> u32;
    pub safe fn TlsFree(index: u32) -> Bool;
    pub safe fn TlsGetValue(index: u32) -> *mut c_void;
    pub safe fn TlsSetValue(index: u32, value: *mut c_void) -> Bool;
    pub safe fn Sleep(milliseconds: u32);
    pub safe fn GetTickCount64() -> u64;
}

pub type WindowProcedure = unsafe extern "system" fn(Handle, u32, usize, isize) -> isize;

pub const WM_CREATE: u32 = 0x0001;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_USER: u32 = 0x0400;
pub const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
pub const CW_USEDEFAULT: i32 = i32::MIN;
pub const SW_SHOW: i32 = 5;
pub const COLOR_WINDOW: usize = 5;
pub const TRANSPARENT: i32 = 1;
pub const BLACK_PEN: i32 = 7;
pub const IDOK: i32 = 1;
pub const OPAQUE: i32 = 2;
pub const PM_NOREMOVE: u32 = 0;
pub const ERROR_CANNOT_FIND_WND_CLASS: u32 = 1407;
pub const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

/// `RGB(r, g, b)`: a `COLORREF`.
pub const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | (green as u32) << 8 | (blue as u32) << 16
}

#[repr(C)]
pub struct WndClassA {
    pub style: u32,
    pub window_procedure: Option<WindowProcedure>,
    pub class_extra: i32,
    pub window_extra: i32,
    pub instance: *mut c_void,
    pub icon: Handle,
    pub cursor: Handle,
    pub background: usize,
    pub menu_name: *const c_char,
    pub class_name: *const c_char,
}

#[repr(C)]
pub struct WndClassExA {
    pub size: u32,
    pub style: u32,
    pub window_procedure: Option<WindowProcedure>,
    pub class_extra: i32,
    pub window_extra: i32,
    pub instance: *mut c_void,
    pub icon: Handle,
    pub cursor: Handle,
    pub background: usize,
    pub menu_name: *const c_char,
    pub class_name: *const c_char,
    pub small_icon: Handle,
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Size {
    pub cx: i32,
    pub cy: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct Msg {
    pub window: Handle,
    pub message: u32,
    pub wparam: usize,
    pub lparam: isize,
    pub time: u32,
    pub point: Point,
    pub private: u32,
}

#[repr(C)]
pub struct CreateStructA {
    pub create_params: *mut c_void,
    pub instance: *mut c_void,
    pub menu: Handle,
    pub parent: Handle,
    pub cy: i32,
    pub cx: i32,
    pub y: i32,
    pub x: i32,
    pub style: i32,
    pub name: *const c_char,
    pub class: *const c_char,
    pub ex_style: u32,
}

#[repr(C)]
pub struct PaintStruct {
    pub context: Handle,
    pub erase: Bool,
    pub paint: Rect,
    pub restore: Bool,
    pub inc_update: Bool,
    pub reserved: [u8; 32],
}

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    pub fn RegisterClassA(class: *const WndClassA) -> u16;
    pub fn RegisterClassExA(class: *const WndClassExA) -> u16;
    pub safe fn GetSystemMetrics(index: i32) -> i32;
    pub fn CreateWindowExA(
        ex_style: u32,
        class_name: *const c_char,
        window_name: *const c_char,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: *mut c_void,
        param: *mut c_void,
    ) -> Handle;
    pub safe fn ShowWindow(window: Handle, command: i32) -> Bool;
    pub safe fn UpdateWindow(window: Handle) -> Bool;
    pub fn GetMessageA(message: *mut Msg, window: Handle, min: u32, max: u32) -> Bool;
    pub fn PeekMessageA(message: *mut Msg, window: Handle, min: u32, max: u32, remove: u32)
    -> Bool;
    pub fn TranslateMessage(message: *const Msg) -> Bool;
    pub fn DispatchMessageA(message: *const Msg) -> isize;
    pub safe fn DefWindowProcA(window: Handle, message: u32, wparam: usize, lparam: isize)
    -> isize;
    pub safe fn DestroyWindow(window: Handle) -> Bool;
    pub safe fn PostQuitMessage(exit_code: i32);
    pub safe fn PostMessageA(window: Handle, message: u32, wparam: usize, lparam: isize) -> Bool;
    pub safe fn SendMessageA(window: Handle, message: u32, wparam: usize, lparam: isize) -> isize;
    pub fn BeginPaint(window: Handle, paint: *mut PaintStruct) -> Handle;
    pub fn EndPaint(window: Handle, paint: *const PaintStruct) -> Bool;
    pub safe fn GetDC(window: Handle) -> Handle;
    pub safe fn ReleaseDC(window: Handle, context: Handle) -> i32;
    pub fn GetClientRect(window: Handle, rect: *mut Rect) -> Bool;
    pub fn InvalidateRect(window: Handle, rect: *const Rect, erase: Bool) -> Bool;
    pub fn SetWindowTextA(window: Handle, title: *const c_char) -> Bool;
    pub fn FillRect(context: Handle, rect: *const Rect, brush: Handle) -> i32;
    pub fn MessageBoxA(
        owner: Handle,
        text: *const c_char,
        caption: *const c_char,
        kind: u32,
    ) -> i32;
    pub fn LoadCursorA(instance: *mut c_void, name: *const c_char) -> Handle;
}

#[link(name = "gdi32", kind = "raw-dylib")]
unsafe extern "system" {
    pub fn TextOutA(context: Handle, x: i32, y: i32, text: *const u8, length: i32) -> Bool;
    pub fn GetTextExtentPoint32A(
        context: Handle,
        text: *const u8,
        length: i32,
        size: *mut Size,
    ) -> Bool;
    pub safe fn SetTextColor(context: Handle, color: u32) -> u32;
    pub safe fn SetBkColor(context: Handle, color: u32) -> u32;
    pub safe fn SetBkMode(context: Handle, mode: i32) -> i32;
    pub safe fn CreateSolidBrush(color: u32) -> Handle;
    pub safe fn CreatePen(style: i32, width: i32, color: u32) -> Handle;
    pub safe fn DeleteObject(object: Handle) -> Bool;
    pub safe fn GetStockObject(index: i32) -> Handle;
    pub safe fn SelectObject(context: Handle, object: Handle) -> Handle;
    pub safe fn Rectangle(context: Handle, left: i32, top: i32, right: i32, bottom: i32) -> Bool;
    pub safe fn SetPixel(context: Handle, x: i32, y: i32, color: u32) -> u32;
    pub fn MoveToEx(context: Handle, x: i32, y: i32, previous: *mut Point) -> Bool;
    pub safe fn LineTo(context: Handle, x: i32, y: i32) -> Bool;
}

/// Text of at most `N` bytes, built with `write!`; what does not fit is
/// dropped.
pub struct Text<const N: usize> {
    bytes: [u8; N],
    length: usize,
}

impl<const N: usize> Text<N> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            length: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    pub fn clear(&mut self) {
        self.length = 0;
    }
}

impl<const N: usize> Default for Text<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for Text<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let room = N - self.length;
        let mut take = text.len().min(room);
        while !text.is_char_boundary(take) {
            take -= 1;
        }
        self.bytes[self.length..self.length + take].copy_from_slice(&text.as_bytes()[..take]);
        self.length += take;
        Ok(())
    }
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

/// Counts checks for a test program; a failed check reports itself, with
/// the line it is on and the thread's last error code, and ends the
/// program with exit code 1.
pub struct Checks {
    program: &'static str,
    passed: u32,
}

impl Checks {
    /// Starts counting for the program called `program`.
    pub const fn new(program: &'static str) -> Self {
        Self { program, passed: 0 }
    }

    /// Records that `condition`, described by `what`, holds — or ends the
    /// program if it does not.
    #[track_caller]
    pub fn check(&mut self, condition: bool, what: &str) {
        if !condition {
            let line = core::panic::Location::caller().line();
            say!(
                "{}: FAILED: {what} (line {line}, last error {})",
                self.program,
                GetLastError()
            );
            ExitProcess(1);
        }
        self.passed += 1;
    }

    /// Number of checks passed so far.
    pub const fn count(&self) -> u32 {
        self.passed
    }
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
