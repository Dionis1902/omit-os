// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! x86 port-mapped I/O.

use core::arch::asm;

/// Writes `value` to I/O port `port`.
///
/// # Safety
///
/// The caller must own the device behind `port` and the write must not
/// violate that device's protocol in a way that affects memory safety.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: `out` only touches the named registers; the device-level
    // effects are the caller's responsibility.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value,
             options(nomem, nostack, preserves_flags));
    }
}

/// Reads a byte from I/O port `port`.
///
/// # Safety
///
/// The caller must own the device behind `port`; reads can have side effects
/// on the device.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: `in` only touches the named registers; the device-level
    // effects are the caller's responsibility.
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value,
             options(nomem, nostack, preserves_flags));
    }
    value
}

/// Writes a 32-bit `value` to I/O port `port`.
///
/// # Safety
///
/// Same contract as [`outb`].
#[inline]
pub unsafe fn outl(port: u16, value: u32) {
    // SAFETY: see `outb`.
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value,
             options(nomem, nostack, preserves_flags));
    }
}

/// Reads a 32-bit value from I/O port `port`.
///
/// # Safety
///
/// Same contract as [`inb`].
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    // SAFETY: see `inb`.
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") value,
             options(nomem, nostack, preserves_flags));
    }
    value
}

/// Writes a 16-bit `value` to I/O port `port`.
///
/// # Safety
///
/// Same contract as [`outb`].
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    // SAFETY: see `outb`.
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value,
             options(nomem, nostack, preserves_flags));
    }
}
