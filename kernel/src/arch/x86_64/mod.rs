// SPDX-License-Identifier: GPL-2.0-only
//! x86-64 architecture support.

use core::arch::asm;

use omit_bootproto::{KERNEL_STACK_BOTTOM, KERNEL_STACK_TOP};

pub mod acpi;
pub mod apic;
pub mod context;
pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod ioapic;
pub mod paging;
pub mod pic;
mod pit;
mod port;
pub mod ps2;
pub mod qemu;
pub mod serial;
pub mod syscall;
pub mod uspace;

/// Name of the architecture, for the boot banner.
pub const NAME: &str = "x86_64";

/// Stops the CPU permanently.
///
/// Interrupts are disabled, so `hlt` never returns in practice; the loop
/// covers non-maskable wake-ups.
pub fn halt_forever() -> ! {
    loop {
        // SAFETY: `cli` and `hlt` are valid in ring 0 and touch no memory.
        // Halting with interrupts disabled is the intended terminal state.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// Returns the caller's frame pointer.
#[inline(always)]
pub fn frame_pointer() -> u64 {
    let rbp: u64;
    // SAFETY: copies RBP into a general-purpose register; no side effects.
    unsafe { asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack, preserves_flags)) };
    rbp
}

/// Maximum number of frames [`backtrace`] reports.
const BACKTRACE_LIMIT: usize = 32;

/// Walks the frame-pointer chain starting at `rbp`, calling `emit` with each
/// return address.
///
/// The kernel is compiled with frame pointers, so each frame starts with the
/// saved caller RBP followed by the return address. The walk is defensive:
/// it runs on panic and exception paths where the chain may be corrupt, so a
/// frame is only dereferenced if it lies entirely within a known kernel
/// stack, and the chain must move strictly upwards.
pub fn backtrace(mut rbp: u64, mut emit: impl FnMut(u64)) {
    let df_stack = gdt::double_fault_stack_range();
    let on_known_stack = |frame: u64| {
        let Some(end) = frame.checked_add(16) else {
            return false;
        };
        (frame >= KERNEL_STACK_BOTTOM && end <= KERNEL_STACK_TOP)
            || (frame >= df_stack.start && end <= df_stack.end)
    };

    for _ in 0..BACKTRACE_LIMIT {
        if !rbp.is_multiple_of(8) || !on_known_stack(rbp) {
            return;
        }
        let frame = rbp as *const [u64; 2];
        // SAFETY: `rbp` is 8-byte aligned and `[rbp, rbp + 16)` lies inside
        // the boot stack or the double-fault stack, both of which are mapped
        // readable kernel memory for the lifetime of the kernel. Any bit
        // pattern is a valid `u64`.
        let [next, return_address] = unsafe { frame.read() };
        if return_address == 0 {
            return;
        }
        emit(return_address);
        if next <= rbp {
            return;
        }
        rbp = next;
    }
}

/// Deliberately reads from an unmapped address to raise a page fault.
/// Test-only; see `fault=pagefault` on the kernel command line.
pub fn trigger_page_fault(address: u64) {
    // SAFETY: this is an intentional invalid access performed in assembly
    // (so it is a CPU fault, not Rust-level undefined behaviour). The caller
    // passes an address that is unmapped; the resulting #PF is fatal and
    // handled by the exception path, so execution never continues with a
    // bogus value.
    unsafe {
        asm!("mov {tmp}, byte ptr [{addr}]", addr = in(reg) address, tmp = out(reg_byte) _,
             options(readonly, nostack, preserves_flags));
    }
}
