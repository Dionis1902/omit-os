// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! `syscheck` — checks the CPU state the kernel hands back to a program
//! after a system call.
//!
//! The values checked here are set by the `sysret` instruction from
//! kernel configuration, and CPU vendors differ in the details (AMD CPUs
//! load the stack segment selector exactly as configured, Intel CPUs and
//! software emulators force its privilege bits). A wrong value is
//! invisible to most programs and only causes a crash later, when an
//! interrupt arrives at the wrong moment — so it is checked explicitly.

#![no_std]
#![no_main]

use core::arch::asm;

use omit_rt::println;

omit_rt::entry!(main);

/// Selectors of user code and data segments (ring 3).
const USER_CS: u16 = 0x23;
const USER_SS: u16 = 0x1B;

const FLAG_DIRECTION: u64 = 1 << 10;
const FLAG_INTERRUPTS: u64 = 1 << 9;
const FLAG_IOPL: u64 = 3 << 12;

fn segments() -> (u16, u16) {
    let (cs, ss): (u16, u16);
    // SAFETY: reads two segment registers into general registers.
    unsafe {
        asm!(
            "mov {cs:x}, cs",
            "mov {ss:x}, ss",
            cs = out(reg) cs,
            ss = out(reg) ss,
            options(nomem, nostack, preserves_flags),
        );
    }
    (cs, ss)
}

fn flags() -> u64 {
    let flags: u64;
    // SAFETY: pushes the flags register and pops it into a register; the
    // stack is balanced again afterwards.
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags)) };
    flags
}

/// Makes a system call with known values in the registers a system call
/// must preserve, and reports whether they all survived.
fn preserved_registers_survive() -> bool {
    const PATTERN: u64 = 0x0123_4567_89AB_CDEF;
    let (mut r12, mut r13, mut r14, mut r15) = (PATTERN, !PATTERN, PATTERN ^ 0xFF, PATTERN << 1);
    let expected = (r12, r13, r14, r15);
    // SAFETY: `getpid` takes no arguments and only returns a number. The
    // registers the kernel is allowed to change are declared as outputs.
    unsafe {
        asm!(
            "syscall",
            inout("rax") omit_rt::abi::nr::GETPID => _,
            inout("r12") r12,
            inout("r13") r13,
            inout("r14") r14,
            inout("r15") r15,
            lateout("rcx") _, lateout("r11") _,
            lateout("rdi") _, lateout("rsi") _, lateout("rdx") _,
            lateout("r8") _, lateout("r9") _, lateout("r10") _,
            options(nostack),
        );
    }
    (r12, r13, r14, r15) == expected
}

fn main() -> i32 {
    let mut failures = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            println!("syscheck: FAILED: {what}");
            failures += 1;
        }
    };

    // The process starts through an interrupt return; the state must be
    // right then, and still right after returning from a system call.
    for stage in ["at program start", "after a system call"] {
        let (cs, ss) = segments();
        if (cs, ss) != (USER_CS, USER_SS) {
            println!("syscheck: {stage}: cs={cs:#06x} ss={ss:#06x}");
        }
        check(cs == USER_CS, "code segment selector");
        check(ss == USER_SS, "stack segment selector");
        let flags = flags();
        check(
            flags & FLAG_INTERRUPTS != 0,
            "interrupts are enabled in user mode",
        );
        check(flags & FLAG_DIRECTION == 0, "direction flag is clear");
        check(flags & FLAG_IOPL == 0, "no I/O privilege");
        // The first pass ends with a system call (`getpid`), so that the
        // second pass sees the state `sysret` produced.
        let _ = omit_rt::getpid();
    }
    check(
        preserved_registers_survive(),
        "system calls preserve r12-r15",
    );

    // Stay in user mode long enough for timer interrupts to arrive after
    // a system call return: this is the situation in which a wrong stack
    // segment selector crashes the machine.
    let start = omit_rt::uptime_ms();
    let mut spins = 0u64;
    while omit_rt::uptime_ms() - start < 60 {
        for _ in 0..20_000 {
            spins = core::hint::black_box(spins + 1);
        }
    }
    let (cs, ss) = segments();
    check(
        (cs, ss) == (USER_CS, USER_SS),
        "segments after being interrupted in user mode",
    );

    if failures == 0 {
        println!("syscheck: all checks passed");
        0
    } else {
        1
    }
}
