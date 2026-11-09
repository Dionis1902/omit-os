// SPDX-License-Identifier: GPL-2.0-only
//! QEMU `isa-debug-exit` support for automated tests.
//!
//! When the kernel command line contains the `qemu-exit` flag, the kernel
//! reports its final status by writing to the debug-exit device, which makes
//! QEMU terminate with exit status `(code << 1) | 1`. The flag must only be
//! given when QEMU was started with
//! `-device isa-debug-exit,iobase=0xf4,iosize=0x04`; it is never enabled
//! implicitly, so on real hardware port 0xF4 is left alone.

use core::sync::atomic::{AtomicBool, Ordering};

use super::port::outl;

const DEBUG_EXIT_PORT: u16 = 0xF4;

/// Final status reported to the test harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ExitCode {
    /// Initialisation completed; QEMU exits with status 33.
    Success = 0x10,
    /// Initialisation failed, or the kernel panicked or took a fatal
    /// exception; QEMU exits with status 35.
    Failure = 0x11,
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Enables [`exit`]. Called when `qemu-exit` is on the command line.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Terminates QEMU with `code` if the debug-exit device was enabled;
/// otherwise returns.
pub fn exit(code: ExitCode) {
    if ENABLED.load(Ordering::Relaxed) {
        // SAFETY: the operator declared via `qemu-exit` that the
        // isa-debug-exit device sits at this port. The write terminates the
        // virtual machine and has no other effect.
        unsafe { outl(DEBUG_EXIT_PORT, code as u32) };
    }
}
