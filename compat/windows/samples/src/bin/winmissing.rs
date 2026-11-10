// SPDX-License-Identifier: GPL-2.0-only
//! A program that needs functions the compatibility layer does not have:
//! a message box (there is no window system) and the current date and
//! time (there is no real-time clock driver). It must be refused before
//! it starts, with both named.

#![no_std]
#![no_main]

use core::ffi::{c_char, c_void};

use omit_win_samples::{ExitProcess, say};

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn MessageBoxA(
        window: *mut c_void,
        text: *const c_char,
        caption: *const c_char,
        kind: u32,
    ) -> i32;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    /// The current date and time: Omit OS has no real-time clock driver
    /// yet, so this function does not exist there.
    fn GetSystemTimeAsFileTime(time: *mut u64);
}

/// Entry point.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    say!("winmissing: started, which must not happen");
    // SAFETY: valid arguments for both calls; never reached on Omit OS.
    unsafe {
        MessageBoxA(
            core::ptr::null_mut(),
            c"text".as_ptr(),
            c"caption".as_ptr(),
            0,
        );
        let mut time = 0u64;
        GetSystemTimeAsFileTime(&raw mut time);
    }
    ExitProcess(1)
}
