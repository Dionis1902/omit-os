// SPDX-License-Identifier: GPL-2.0-only
//! A program that needs things the compatibility layer does not have:
//! a function of a system library that is missing altogether (the
//! registry, `ADVAPI32.dll`), a function that is missing from a library
//! that exists (`USER32.dll` has no clipboard), and starting another
//! program (`CreateProcessA`). It must be refused before
//! it starts, with all three named.

#![no_std]
#![no_main]

use core::ffi::c_void;

use omit_win_samples::{ExitProcess, say};

#[link(name = "advapi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn RegCloseKey(key: *mut c_void) -> i32;
}

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    fn CloseClipboard() -> i32;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    /// Starting another program is not implemented.
    fn CreateProcessA(
        application: *const u8,
        command_line: *mut u8,
        process_attributes: *mut c_void,
        thread_attributes: *mut c_void,
        inherit_handles: i32,
        flags: u32,
        environment: *mut c_void,
        directory: *const u8,
        startup: *mut c_void,
        information: *mut c_void,
    ) -> i32;
}

/// Entry point.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    say!("winmissing: started, which must not happen");
    // SAFETY: valid arguments for the calls; never reached on Omit OS.
    unsafe {
        RegCloseKey(core::ptr::null_mut());
        CloseClipboard();
        CreateProcessA(
            core::ptr::null(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            0,
            0,
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
    }
    ExitProcess(1)
}
