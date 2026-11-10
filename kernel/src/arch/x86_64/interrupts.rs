// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Interrupt masking and dispatch of hardware interrupt vectors.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

use super::apic;
use crate::sync::SpinLock;

/// First vector available for hardware interrupts; 0-31 are CPU exceptions.
pub const FIRST_VECTOR: u8 = 32;
/// Vectors 32-47 are where the legacy PIC is parked (masked). They are not
/// handed out, so a spurious PIC interrupt can never be mistaken for a
/// device.
pub const FIRST_USABLE_VECTOR: u8 = 48;
/// Local APIC spurious-interrupt vector. Delivered without an in-service
/// bit, so it must not be acknowledged.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

const RFLAGS_IF: u64 = 1 << 9;

/// An interrupt handler. Runs with interrupts disabled, on the interrupted
/// context's stack; it must be short and must not block.
pub type Handler = fn();

const SLOTS: usize = 256 - FIRST_VECTOR as usize;
static HANDLERS: SpinLock<[Option<Handler>; SLOTS]> = SpinLock::new([None; SLOTS]);
static UNHANDLED: AtomicU64 = AtomicU64::new(0);

/// Why a handler could not be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    /// The vector is reserved (exception, legacy PIC range or spurious).
    ReservedVector,
    /// Another handler already owns the vector.
    InUse,
}

/// Installs `handler` for `vector`.
pub fn register(vector: u8, handler: Handler) -> Result<(), RegisterError> {
    if !(FIRST_USABLE_VECTOR..SPURIOUS_VECTOR).contains(&vector) {
        return Err(RegisterError::ReservedVector);
    }
    let mut handlers = HANDLERS.lock();
    let slot = &mut handlers[usize::from(vector - FIRST_VECTOR)];
    if slot.is_some() {
        return Err(RegisterError::InUse);
    }
    *slot = Some(handler);
    Ok(())
}

/// Number of interrupts that arrived on vectors without a handler.
pub fn unhandled_count() -> u64 {
    UNHANDLED.load(Ordering::Relaxed)
}

/// Called from the trap entry path for every vector >= 32.
pub(super) fn dispatch(vector: u64) {
    if vector == u64::from(SPURIOUS_VECTOR) {
        return;
    }
    // Copy the handler out so it does not run under the table lock.
    let handler = usize::try_from(vector)
        .ok()
        .and_then(|v| v.checked_sub(usize::from(FIRST_VECTOR)))
        .and_then(|slot| HANDLERS.lock().get(slot).copied().flatten());
    match handler {
        Some(handler) => handler(),
        None => {
            UNHANDLED.fetch_add(1, Ordering::Relaxed);
        }
    }
    apic::end_of_interrupt();
}

/// Whether maskable interrupts are currently enabled on this CPU.
pub fn are_enabled() -> bool {
    let rflags: u64;
    // SAFETY: pushes RFLAGS and pops it into a register; the stack is
    // balanced and nothing else is modified.
    unsafe { asm!("pushfq", "pop {}", out(reg) rflags, options(preserves_flags)) };
    rflags & RFLAGS_IF != 0
}

/// Disables maskable interrupts and reports whether they were enabled.
pub fn save_and_disable() -> bool {
    let enabled = are_enabled();
    disable();
    enabled
}

/// Re-enables interrupts if `were_enabled`; the counterpart of
/// [`save_and_disable`].
pub fn restore(were_enabled: bool) {
    if were_enabled {
        enable();
    }
}

/// Enables maskable interrupts.
///
/// Callers must have installed the IDT and be prepared for handlers to run
/// at any point afterwards; this is not `unsafe` because handlers only touch
/// state behind interrupt-safe locks or atomics.
pub fn enable() {
    // SAFETY: `sti` only sets RFLAGS.IF. The kernel's IDT covers all 256
    // vectors (see `idt::init`), so any interrupt that now arrives is
    // delivered to a valid handler.
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Disables maskable interrupts.
pub fn disable() {
    // SAFETY: `cli` only clears RFLAGS.IF.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Enables interrupts and halts the CPU until the next one arrives.
///
/// `sti` takes effect after the following instruction, so no interrupt can
/// slip in between the two and leave the CPU halted with nothing to wake it.
pub fn wait() {
    // SAFETY: as for `enable`; `hlt` merely idles the CPU until an
    // interrupt.
    unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
}
