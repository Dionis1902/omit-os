// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Random numbers for programs.
//!
//! The generator is `omit_rng::Generator`. This module is where it gets
//! its unpredictability from:
//!
//! * the processor's random number instruction (`RDRAND`), where the
//!   processor has one: 32 bytes when the generator is set up and again
//!   on every request;
//! * otherwise only **timing**: the time-stamp counter read at every
//!   timer tick and every input event, and the clock. How unpredictable
//!   that is has not been measured, and in a virtual machine it may be
//!   very little. Requests then say that the numbers are of this weaker
//!   kind, so that a program can tell its user.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use omit_rng::Generator;

use crate::arch::cpu;
use crate::sync::Mutex;

static GENERATOR: Mutex<Generator> = Mutex::new(Generator::new());
/// The processor has `RDRAND`, and it worked when tried.
static HARDWARE: AtomicBool = AtomicBool::new(false);
/// Time-stamp counter values, folded together as they are noted.
static TIMING: AtomicU64 = AtomicU64::new(0);

/// One 64-bit value from `RDRAND`, if the instruction delivers one
/// within a few tries (it may fail when the processor's generator is
/// drained).
///
/// # Safety
///
/// The processor must have `RDRAND` (CPUID leaf 1, ECX bit 30).
#[target_feature(enable = "rdrand")]
unsafe fn rdrand() -> Option<u64> {
    for _ in 0..10 {
        let mut value = 0;
        // (Safe to call here: this function is compiled for a processor
        // with the instruction, which its caller vouches for.)
        if core::arch::x86_64::_rdrand64_step(&mut value) == 1 {
            return Some(value);
        }
    }
    None
}

/// 32 bytes from the processor, if it has them to give.
fn from_hardware() -> Option<[u8; 32]> {
    if cpu::cpuid(1, 0).2 & (1 << 30) == 0 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for chunk in bytes.chunks_exact_mut(8) {
        // SAFETY: CPUID said the instruction exists.
        chunk.copy_from_slice(&unsafe { rdrand() }?.to_le_bytes());
    }
    Some(bytes)
}

/// Notes the moment something happened whose timing nobody chose: a
/// timer tick, a key, a mouse movement. Cheap, and safe in interrupt
/// handlers.
pub fn note_timing() {
    let now = cpu::read_tsc();
    let _ = TIMING.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
        Some(old.rotate_left(7) ^ now)
    });
}

/// Sets the generator up. Returns whether the processor supplied random
/// numbers (`false`: timing is all there is).
pub fn init() -> bool {
    let hardware = from_hardware();
    let mut generator = GENERATOR.lock();
    if let Some(bytes) = hardware {
        generator.mix(&bytes);
    }
    generator.mix(&cpu::read_tsc().to_le_bytes());
    if let Some(now) = crate::time::realtime_us() {
        generator.mix(&now.to_le_bytes());
    }
    HARDWARE.store(hardware.is_some(), Ordering::Relaxed);
    hardware.is_some()
}

/// Fills `out` with random bytes. Returns whether the processor's
/// random numbers went into them; if not, they rest on timing alone.
pub fn fill(out: &mut [u8]) -> bool {
    // Fresh input before every request: the processor's, if any, and
    // whatever timing has been noted, with the present moment.
    let hardware = if HARDWARE.load(Ordering::Relaxed) {
        from_hardware()
    } else {
        None
    };
    note_timing();
    let timing = TIMING.load(Ordering::Relaxed);
    let mut generator = GENERATOR.lock();
    if let Some(bytes) = hardware {
        generator.mix(&bytes);
    }
    generator.mix(&timing.to_le_bytes());
    generator.fill(out);
    hardware.is_some()
}
