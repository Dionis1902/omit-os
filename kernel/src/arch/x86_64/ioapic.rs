// SPDX-License-Identifier: GPL-2.0-only
//! I/O APIC: routes device interrupt lines to local APIC vectors.
//!
//! Only the first I/O APIC is used, which covers all legacy ISA interrupts
//! on PC-compatible machines.

use core::fmt;

use omit_acpi::IsaRoute;

use super::paging::{self, MapError};
use crate::sync::SpinLock;

const REG_SELECT: u64 = 0x00;
const REG_WINDOW: u64 = 0x10;

const INDEX_VERSION: u32 = 0x01;
const INDEX_REDIRECTION: u32 = 0x10;

const REDIRECT_ACTIVE_LOW: u32 = 1 << 13;
const REDIRECT_LEVEL: u32 = 1 << 15;
const REDIRECT_MASKED: u32 = 1 << 16;

/// Why an I/O APIC operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoApicError {
    /// The register page could not be mapped.
    Map(MapError),
    /// [`init`] has not been called.
    NotInitialised,
    /// The interrupt is not handled by this I/O APIC.
    GsiOutOfRange {
        /// The offending global system interrupt.
        gsi: u32,
    },
}

impl fmt::Display for IoApicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Map(e) => write!(f, "cannot map I/O APIC: {e}"),
            Self::NotInitialised => write!(f, "I/O APIC is not initialised"),
            Self::GsiOutOfRange { gsi } => write!(f, "GSI {gsi} is not on the I/O APIC"),
        }
    }
}

struct IoApic {
    /// Virtual address of the register page.
    base: u64,
    gsi_base: u32,
    /// Number of redirection entries.
    lines: u32,
}

impl IoApic {
    fn read(&mut self, index: u32) -> u32 {
        // SAFETY: `base` is the device mapping of the I/O APIC register
        // page; the select and window registers are 32-bit and lie at
        // aligned offsets within it. `&mut self` (held under the lock)
        // keeps the select/window pair from being interleaved.
        unsafe {
            ((self.base + REG_SELECT) as *mut u32).write_volatile(index);
            ((self.base + REG_WINDOW) as *const u32).read_volatile()
        }
    }

    fn write(&mut self, index: u32, value: u32) {
        // SAFETY: as in `read`.
        unsafe {
            ((self.base + REG_SELECT) as *mut u32).write_volatile(index);
            ((self.base + REG_WINDOW) as *mut u32).write_volatile(value);
        }
    }

    /// Programs redirection entry `line`. The high half is written first so
    /// the entry is never live with a stale destination.
    fn set_redirection(&mut self, line: u32, low: u32, high: u32) {
        let index = INDEX_REDIRECTION + 2 * line;
        self.write(index, REDIRECT_MASKED);
        self.write(index + 1, high);
        self.write(index, low);
    }
}

static IOAPIC: SpinLock<Option<IoApic>> = SpinLock::new(None);

/// Maps the I/O APIC at physical address `address` and masks all of its
/// interrupt lines. Returns the number of lines.
pub fn init(address: u32, gsi_base: u32) -> Result<u32, IoApicError> {
    let base = paging::map_mmio(u64::from(address), 0x20).map_err(IoApicError::Map)?;
    let mut ioapic = IoApic {
        base,
        gsi_base,
        lines: 0,
    };
    ioapic.lines = ((ioapic.read(INDEX_VERSION) >> 16) & 0xFF) + 1;
    for line in 0..ioapic.lines {
        ioapic.set_redirection(line, REDIRECT_MASKED, 0);
    }
    let lines = ioapic.lines;
    *IOAPIC.lock() = Some(ioapic);
    Ok(lines)
}

/// Routes the interrupt described by `route` to `vector` on the CPU whose
/// local APIC ID is `apic_id`, and unmasks it.
pub fn enable(route: IsaRoute, vector: u8, apic_id: u8) -> Result<(), IoApicError> {
    let mut guard = IOAPIC.lock();
    let ioapic = guard.as_mut().ok_or(IoApicError::NotInitialised)?;
    let line = route
        .gsi
        .checked_sub(ioapic.gsi_base)
        .filter(|&line| line < ioapic.lines)
        .ok_or(IoApicError::GsiOutOfRange { gsi: route.gsi })?;

    // Fixed delivery, physical destination mode.
    let mut low = u32::from(vector);
    if route.active_low {
        low |= REDIRECT_ACTIVE_LOW;
    }
    if route.level_triggered {
        low |= REDIRECT_LEVEL;
    }
    ioapic.set_redirection(line, low, u32::from(apic_id) << 24);
    Ok(())
}
