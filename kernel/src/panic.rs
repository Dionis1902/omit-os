// SPDX-License-Identifier: GPL-2.0-only
//! Kernel panic handling.
//!
//! A panic is unrecoverable: the handler prints what went wrong, where, and
//! how execution got there, reports failure to the test harness if one is
//! attached, and halts the CPU.

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{self, qemu};
use crate::kprintln;

static PANICKING: AtomicBool = AtomicBool::new(false);

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    arch::interrupts::disable();
    // A panic raised while already reporting one (for example from a broken
    // `Display` impl) must not recurse forever.
    if PANICKING.swap(true, Ordering::AcqRel) {
        qemu::exit(qemu::ExitCode::Failure);
        arch::halt_forever();
    }

    crate::fbcon::resume_for_emergency();
    kprintln!();
    kprintln!("==== KERNEL PANIC ====");
    kprintln!("message: {}", info.message());
    match info.location() {
        Some(location) => kprintln!(
            "location: {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        ),
        None => kprintln!("location: unknown"),
    }
    kprintln!("backtrace:");
    let mut depth = 0;
    arch::backtrace(arch::frame_pointer(), |address| {
        kprintln!("  #{depth} {address:#018x}");
        depth += 1;
    });
    kprintln!("======================");

    qemu::exit(qemu::ExitCode::Failure);
    arch::halt_forever();
}
