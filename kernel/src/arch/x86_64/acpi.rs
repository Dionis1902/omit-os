// SPDX-License-Identifier: GPL-2.0-only
//! Locating ACPI tables in physical memory.
//!
//! Parsing and validation are done by the `omit_acpi` crate; this module
//! only turns the physical addresses found in the tables into byte slices
//! through the direct map.

use core::fmt;

use omit_acpi::{AcpiError, IsaRoute, Madt, MadtEntry, RSDP_V1_SIZE, RSDP_V2_SIZE, Rsdp, Table};

use crate::mm::phys_to_virt;

/// Why the interrupt configuration could not be read from ACPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiLookupError {
    /// The firmware did not provide an RSDP.
    NoRsdp,
    /// A table lies outside the direct map.
    Unreachable {
        /// Physical address of the table.
        phys: u64,
    },
    /// A table failed validation.
    Invalid(AcpiError),
    /// There is no MADT.
    NoMadt,
    /// The MADT lists no I/O APIC.
    NoIoApic,
}

impl fmt::Display for AcpiLookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRsdp => write!(f, "firmware provided no ACPI RSDP"),
            Self::Unreachable { phys } => {
                write!(f, "ACPI table at {phys:#x} is outside the direct map")
            }
            Self::Invalid(e) => write!(f, "invalid ACPI table: {e}"),
            Self::NoMadt => write!(f, "no MADT found"),
            Self::NoIoApic => write!(f, "MADT lists no I/O APIC"),
        }
    }
}

/// Returns `len` bytes of firmware memory at physical address `phys`.
///
/// # Safety
///
/// `[phys, phys + len)` must be memory that is never written while the
/// returned slice is alive. ACPI tables qualify: they live in
/// ACPI-reclaimable or NVS memory, which the kernel never allocates or
/// modifies.
unsafe fn firmware_bytes(phys: u64, len: usize) -> Result<&'static [u8], AcpiLookupError> {
    let ptr = phys_to_virt(phys, len as u64).ok_or(AcpiLookupError::Unreachable { phys })?;
    // SAFETY: the range lies inside the direct map (checked above), which
    // maps it readable; immutability is guaranteed by the caller. `u8` has
    // no alignment or validity requirements.
    Ok(unsafe { core::slice::from_raw_parts(ptr, len) })
}

/// Maps and validates the system description table at `phys`.
///
/// # Safety
///
/// `phys` must be the address of an ACPI table provided by the firmware.
unsafe fn table_at(phys: u64) -> Result<Table<'static>, AcpiLookupError> {
    // SAFETY: forwarded; the header is read first to learn the full length.
    let header = unsafe { firmware_bytes(phys, omit_acpi::SDT_HEADER_SIZE)? };
    let length = omit_acpi::table_length(header).map_err(AcpiLookupError::Invalid)?;
    // SAFETY: forwarded.
    let bytes = unsafe { firmware_bytes(phys, length)? };
    Table::parse(bytes).map_err(AcpiLookupError::Invalid)
}

/// The interrupt-routing facts the kernel needs from the MADT.
#[derive(Debug, Clone, Copy)]
pub struct InterruptConfig {
    /// Physical address of the first I/O APIC's registers.
    pub ioapic_address: u32,
    /// First global system interrupt handled by that I/O APIC.
    pub ioapic_gsi_base: u32,
    /// Number of usable processors listed.
    pub cpu_count: usize,
    /// Routing of the COM1 serial port interrupt (ISA IRQ 4).
    pub serial_irq: IsaRoute,
    /// Routing of the PS/2 keyboard interrupt (ISA IRQ 1).
    pub keyboard_irq: IsaRoute,
    /// Routing of the PS/2 mouse interrupt (ISA IRQ 12).
    pub mouse_irq: IsaRoute,
}

/// ISA IRQ line of the PS/2 keyboard.
const KEYBOARD_IRQ: u8 = 1;
const MOUSE_IRQ: u8 = 12;
/// ISA IRQ line of the first serial port.
const COM1_IRQ: u8 = 4;

/// Reads the interrupt configuration from the ACPI tables rooted at the
/// RSDP at physical address `rsdp_phys` (0 meaning "none").
///
/// # Safety
///
/// `rsdp_phys` must be 0 or the RSDP address reported by the firmware, and
/// the direct map must be registered with [`crate::mm::init_direct_map`].
pub unsafe fn interrupt_config(rsdp_phys: u64) -> Result<InterruptConfig, AcpiLookupError> {
    if rsdp_phys == 0 {
        return Err(AcpiLookupError::NoRsdp);
    }
    // SAFETY: the RSDP is firmware memory per the function contract. Only
    // the ACPI 1.0 part is read until the revision is known.
    let revision = unsafe { firmware_bytes(rsdp_phys, RSDP_V1_SIZE)? }[15];
    let rsdp_len = if revision >= 2 {
        RSDP_V2_SIZE
    } else {
        RSDP_V1_SIZE
    };
    // SAFETY: as above.
    let rsdp = Rsdp::parse(unsafe { firmware_bytes(rsdp_phys, rsdp_len)? })
        .map_err(AcpiLookupError::Invalid)?;

    let root_phys = rsdp.xsdt_address.unwrap_or(u64::from(rsdp.rsdt_address));
    // SAFETY: the address comes from the validated RSDP.
    let root = unsafe { table_at(root_phys)? };

    for phys in root.root_entries().map_err(AcpiLookupError::Invalid)? {
        // SAFETY: the address comes from the validated root table. Tables
        // that cannot be read or validated are skipped: only the MADT
        // matters here.
        let Ok(table) = (unsafe { table_at(phys) }) else {
            continue;
        };
        let Ok(madt) = Madt::parse(&table) else {
            continue;
        };

        let ioapic = madt.entries().find_map(|entry| match entry {
            MadtEntry::IoApic {
                address, gsi_base, ..
            } => Some((address, gsi_base)),
            _ => None,
        });
        let (ioapic_address, ioapic_gsi_base) = ioapic.ok_or(AcpiLookupError::NoIoApic)?;
        return Ok(InterruptConfig {
            ioapic_address,
            ioapic_gsi_base,
            cpu_count: madt
                .entries()
                .filter(|e| matches!(e, MadtEntry::LocalApic { enabled: true, .. }))
                .count(),
            serial_irq: madt.isa_route(COM1_IRQ),
            keyboard_irq: madt.isa_route(KEYBOARD_IRQ),
            mouse_irq: madt.isa_route(MOUSE_IRQ),
        });
    }
    Err(AcpiLookupError::NoMadt)
}
