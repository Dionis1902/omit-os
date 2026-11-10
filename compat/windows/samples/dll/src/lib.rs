// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! `winsample.dll` — a Windows library for testing the loader: it exports
//! functions, has an initialisation routine, imports from `KERNEL32.dll`
//! itself, and contains data that must be relocated.

#![no_std]

use core::ffi::{CStr, c_char, c_void};
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use omit_win_samples::{GetStdHandle, STD_OUTPUT_HANDLE, WriteFile};

/// How often the library was told that it was loaded.
static ATTACHED: AtomicU32 = AtomicU32::new(0);
/// The module handle the loader passed.
static MODULE: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
/// Whether it was loaded together with the program (rather than later,
/// by `LoadLibrary`).
static WITH_PROGRAM: AtomicU32 = AtomicU32::new(0);

const DLL_PROCESS_ATTACH: u32 = 1;

/// The library's entry point: called by the loader when the library is
/// loaded (and, on Windows, at other times this library ignores).
#[unsafe(no_mangle)]
pub extern "system" fn _DllMainCRTStartup(
    module: *mut c_void,
    reason: u32,
    reserved: *mut c_void,
) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        ATTACHED.fetch_add(1, Ordering::Relaxed);
        MODULE.store(module, Ordering::Relaxed);
        WITH_PROGRAM.store(u32::from(!reserved.is_null()), Ordering::Relaxed);
    }
    1
}

/// Adds two numbers.
#[unsafe(no_mangle)]
pub extern "system" fn sample_add(a: i32, b: i32) -> i32 {
    a + b
}

/// How often the initialisation routine ran.
#[unsafe(no_mangle)]
pub extern "system" fn sample_attach_count() -> u32 {
    ATTACHED.load(Ordering::Relaxed)
}

/// Whether the library was loaded together with the program.
#[unsafe(no_mangle)]
pub extern "system" fn sample_loaded_with_program() -> u32 {
    WITH_PROGRAM.load(Ordering::Relaxed)
}

/// The module handle the initialisation routine received.
#[unsafe(no_mangle)]
pub extern "system" fn sample_module() -> *mut c_void {
    MODULE.load(Ordering::Relaxed)
}

/// Pointers stored in data: correct only if the loader relocated the
/// library.
static GREETINGS: [&CStr; 2] = [c"greetings from winsample.dll", c"second greeting"];

/// A string from the library's data.
#[unsafe(no_mangle)]
pub extern "system" fn sample_greeting(index: u32) -> *const c_char {
    core::hint::black_box(&GREETINGS)
        .get(index as usize)
        .map_or(core::ptr::null(), |text| text.as_ptr())
}

/// Prints `text` with a prefix, through the library's own imports from
/// `KERNEL32.dll`. Returns whether that worked.
///
/// # Safety
///
/// `text` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn sample_print(text: *const c_char) -> i32 {
    // SAFETY: guaranteed by the caller.
    let text = unsafe { CStr::from_ptr(text) }.to_bytes();
    let mut all = 1;
    for part in [b"[winsample.dll] ".as_slice(), text, b"\r\n"] {
        let mut written = 0;
        // SAFETY: a live buffer of the stated length.
        let ok = unsafe {
            WriteFile(
                GetStdHandle(STD_OUTPUT_HANDLE),
                part.as_ptr(),
                part.len() as u32,
                &raw mut written,
                core::ptr::null_mut(),
            )
        };
        all &= i32::from(ok != 0 && written as usize == part.len());
    }
    all
}
