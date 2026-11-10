// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Checks that SSE registers survive being preempted.
//!
//! `winfloat N [MS]` keeps values derived from `N` in two SSE registers
//! for `MS` milliseconds (1500 by default) and verifies them continuously. Two
//! instances with different `N` running at the same time each see only
//! their own values if, and only if, the kernel saves and restores the
//! registers when it switches between them.

#![no_std]
#![no_main]

use core::arch::asm;

use omit_win_samples::{GetTickCount64, command_line, say};

const DEFAULT_DURATION_MS: u64 = 1500;
const BATCH: u64 = 200_000;

/// Holds `seed` in XMM6 and its complement in XMM15 for `iterations`
/// loop turns, comparing both each time. Returns whether they kept their
/// values.
fn hold(seed: u64, iterations: u64) -> bool {
    let intact: u64;
    // SAFETY: the code only uses the registers named as operands or
    // clobbers and touches no memory.
    unsafe {
        asm!(
            "movq xmm6, {seed}",
            "movq xmm15, {inverse}",
            "2:",
            "movq {scratch}, xmm6",
            "cmp {scratch}, {seed}",
            "jne 3f",
            "movq {scratch}, xmm15",
            "cmp {scratch}, {inverse}",
            "jne 3f",
            "dec {count}",
            "jnz 2b",
            "mov {scratch}, 1",
            "jmp 4f",
            "3:",
            "xor {scratch:e}, {scratch:e}",
            "4:",
            seed = in(reg) seed,
            inverse = in(reg) !seed,
            count = inout(reg) iterations => _,
            scratch = out(reg) intact,
            out("xmm6") _,
            out("xmm15") _,
            options(nomem, nostack),
        );
    }
    intact == 1
}

/// Entry point.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    // The program's own name comes first on the command line.
    let mut arguments = command_line().split(' ').skip(1);
    let mut number = || arguments.next().and_then(|text| text.parse::<u64>().ok());
    let id = number().unwrap_or(0);
    let duration = number().unwrap_or(DEFAULT_DURATION_MS);
    // Spread the ID over all 64 bits so that instances differ everywhere.
    let seed = (id + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);

    let start = GetTickCount64();
    let mut batches = 0u64;
    while GetTickCount64() - start < duration {
        if !hold(seed, BATCH) {
            say!("winfloat {id}: SSE REGISTERS CORRUPTED after {batches} batches");
            return 1;
        }
        batches += 1;
    }
    // Floating-point arithmetic through the same registers: the harmonic
    // sum H(1000) = 7.485470860550345 (to 16 digits).
    let mut sum = 0.0f64;
    for term in 1..=1000u32 {
        sum += 1.0 / f64::from(term);
    }
    if (sum - 7.485_470_860_550_345).abs() > 1e-12 {
        say!("winfloat {id}: WRONG ARITHMETIC");
        return 1;
    }
    say!(
        "winfloat {id}: registers intact, H(1000) = 7.{}",
        ((sum - 7.0) * 1e6) as u32
    );
    0
}
