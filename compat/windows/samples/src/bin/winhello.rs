// SPDX-License-Identifier: GPL-2.0-only
//! The smallest useful Windows program: writes a line and exits.

#![no_std]
#![no_main]

use omit_win_samples::{ExitProcess, GetStdHandle, STD_OUTPUT_HANDLE, WriteFile};

/// Entry point of a console program built without the C runtime.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    let message = b"Hello from a Windows executable!\r\n";
    let mut written = 0;
    // SAFETY: the buffer and `written` are live for the call.
    unsafe {
        WriteFile(
            GetStdHandle(STD_OUTPUT_HANDLE),
            message.as_ptr(),
            message.len() as u32,
            &raw mut written,
            core::ptr::null_mut(),
        );
    }
    ExitProcess(0)
}
