// SPDX-License-Identifier: GPL-2.0-only
//! A program that needs functions the compatibility layer does not have:
//! a message box (there is no window system) and a second thread. It must
//! be refused before it starts, with both functions named.

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
    fn CreateThread(
        security: *mut c_void,
        stack_size: usize,
        start: *mut c_void,
        parameter: *mut c_void,
        flags: u32,
        id: *mut u32,
    ) -> *mut c_void;
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
        CreateThread(
            core::ptr::null_mut(),
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            0,
            core::ptr::null_mut(),
        );
    }
    ExitProcess(1)
}
