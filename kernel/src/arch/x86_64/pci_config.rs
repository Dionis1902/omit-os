// SPDX-License-Identifier: GPL-2.0-only
//! PCI configuration space access through I/O ports 0xCF8/0xCFC
//! ("configuration mechanism #1").
//!
//! This reaches the first 256 bytes of every function's configuration
//! space on every PC-compatible machine, which is all the kernel needs so
//! far. The memory-mapped mechanism (ECAM, located through the ACPI MCFG
//! table) is required for PCI Express extended capabilities and will be
//! added when something needs those.

use super::port::{inl, outl};
use crate::sync::SpinLock;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;
const ENABLE: u32 = 1 << 31;

/// Serialises the address/data port pair.
static PORTS: SpinLock<()> = SpinLock::new(());

fn address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    ENABLE
        | (u32::from(bus) << 16)
        | (u32::from(device & 0x1F) << 11)
        | (u32::from(function & 0x7) << 8)
        | u32::from(offset & 0xFC)
}

/// Reads the 32-bit configuration register at `offset` (rounded down to a
/// multiple of 4). A function that does not exist reads as all ones.
pub fn read(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let _guard = PORTS.lock();
    // SAFETY: ports 0xCF8 and 0xCFC are the PCI configuration address and
    // data ports on PC-compatible hardware. Reading configuration space
    // has no side effects on memory safety; the lock keeps the two-step
    // sequence atomic.
    unsafe {
        outl(CONFIG_ADDRESS, address(bus, device, function, offset));
        inl(CONFIG_DATA)
    }
}

/// Writes the 32-bit configuration register at `offset`.
///
/// # Safety
///
/// Configuration writes reprogram the device (address decoding, bus
/// mastering, interrupts). The caller must own the device and write values
/// that are valid for the register.
pub unsafe fn write(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let _guard = PORTS.lock();
    // SAFETY: as in `read`; the effect on the device is the caller's
    // responsibility.
    unsafe {
        outl(CONFIG_ADDRESS, address(bus, device, function, offset));
        outl(CONFIG_DATA, value);
    }
}
