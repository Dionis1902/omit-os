// SPDX-License-Identifier: GPL-2.0-only
//! A program that needs things the compatibility layer does not have:
//! a function of a system library that is missing altogether (the
//! registry, `ADVAPI32.dll`), a function that is missing from a library
//! that exists (`USER32.dll` has no timers), and the current date and
//! time (there is no real-time clock driver). It must be refused before
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
    fn KillTimer(window: *mut c_void, id: usize) -> i32;
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
    // SAFETY: valid arguments for the calls; never reached on Omit OS.
    unsafe {
        RegCloseKey(core::ptr::null_mut());
        KillTimer(core::ptr::null_mut(), 1);
        let mut time = 0u64;
        GetSystemTimeAsFileTime(&raw mut time);
    }
    ExitProcess(1)
}
