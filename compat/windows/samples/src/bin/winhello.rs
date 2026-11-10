// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// Sources: an original test program; Microsoft Learn reference pages of the
//   functions it calls. It is part of the behaviour tests of the Windows
//   compatibility layer (QEMU tests windows-programs and desktop).
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
