// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! A program that needs things the compatibility layer does not have:
//! a function of a system library that is missing altogether (the
//! registry, `ADVAPI32.dll`), a function that is missing from a library
//! that exists (`USER32.dll` has no clipboard), and named pipes
//! (`CreateNamedPipeA`). It must be refused before
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
    /// Named pipes are not implemented.
    fn CreateNamedPipeA(
        name: *const u8,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer_size: u32,
        in_buffer_size: u32,
        default_timeout: u32,
        security: *mut c_void,
    ) -> *mut c_void;
}

/// Entry point.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    say!("winmissing: started, which must not happen");
    // SAFETY: valid arguments for the calls; never reached on Omit OS.
    unsafe {
        RegCloseKey(core::ptr::null_mut());
        CloseClipboard();
        CreateNamedPipeA(core::ptr::null(), 0, 0, 0, 0, 0, 0, core::ptr::null_mut());
    }
    ExitProcess(1)
}
