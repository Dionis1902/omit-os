// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! PCI device discovery.
//!
//! Scans every bus, device and function once at boot and keeps the list.
//! Drivers look their device up by vendor and device ID.

use alloc::vec::Vec;
use core::fmt;

use crate::arch::pci_config;
use crate::sync::SpinLock;

const VENDOR_NONE: u16 = 0xFFFF;
const REG_ID: u8 = 0x00;
const REG_COMMAND: u8 = 0x04;
const REG_CLASS: u8 = 0x08;
const REG_HEADER: u8 = 0x0C;
const REG_BAR0: u8 = 0x10;
const REG_CAPABILITIES: u8 = 0x34;

const STATUS_CAPABILITIES: u32 = 1 << 20;
const HEADER_MULTIFUNCTION: u32 = 1 << 23;
/// Command register: respond to memory-space accesses.
pub const COMMAND_MEMORY: u32 = 1 << 1;
/// Command register: respond to I/O port accesses.
pub const COMMAND_IO: u32 = 1 << 0;
/// Command register: allow the device to initiate DMA.
pub const COMMAND_BUS_MASTER: u32 = 1 << 2;

/// A PCI function found during the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciDevice {
    /// Bus number.
    pub bus: u8,
    /// Device number on the bus (0-31).
    pub device: u8,
    /// Function number (0-7).
    pub function: u8,
    /// Vendor ID.
    pub vendor_id: u16,
    /// Device ID.
    pub device_id: u16,
    /// Base class code.
    pub class: u8,
    /// Subclass code.
    pub subclass: u8,
    /// Programming interface.
    pub prog_if: u8,
}

impl fmt::Display for PciDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}.{} {:04x}:{:04x} {}",
            self.bus,
            self.device,
            self.function,
            self.vendor_id,
            self.device_id,
            class_name(self.class, self.subclass)
        )
    }
}

/// A human-readable name for a class/subclass pair.
pub fn class_name(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x00, _) => "Unclassified device",
        (0x01, 0x00) => "SCSI storage controller",
        (0x01, 0x01) => "IDE controller",
        (0x01, 0x06) => "SATA controller",
        (0x01, 0x08) => "NVMe controller",
        (0x01, _) => "Mass storage controller",
        (0x02, 0x00) => "Ethernet controller",
        (0x02, _) => "Network controller",
        (0x03, 0x00) => "VGA compatible controller",
        (0x03, _) => "Display controller",
        (0x04, _) => "Multimedia controller",
        (0x05, _) => "Memory controller",
        (0x06, 0x00) => "Host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI bridge",
        (0x06, _) => "Bridge",
        (0x07, _) => "Communication controller",
        (0x08, _) => "System peripheral",
        (0x09, _) => "Input device controller",
        (0x0C, 0x03) => "USB controller",
        (0x0C, 0x05) => "SMBus controller",
        (0x0C, _) => "Serial bus controller",
        (0x0D, _) => "Wireless controller",
        _ => "Other device",
    }
}

/// A decoded memory base address register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBar {
    /// Physical address the device's registers are mapped at.
    pub address: u64,
}

impl PciDevice {
    fn read(&self, offset: u8) -> u32 {
        pci_config::read(self.bus, self.device, self.function, offset)
    }

    /// Reads a 32-bit configuration register.
    pub fn config_u32(&self, offset: u8) -> u32 {
        self.read(offset)
    }

    /// Reads an 8-bit configuration register.
    pub fn config_u8(&self, offset: u8) -> u8 {
        (self.read(offset) >> (8 * u32::from(offset & 3))) as u8
    }

    /// The raw value of base address register `index` (0-5), for
    /// diagnostics.
    pub fn bar_raw(&self, index: u8) -> u32 {
        if index > 5 {
            return 0;
        }
        self.read(REG_BAR0 + 4 * index)
    }

    /// Decodes base address register `index` (0-5) if it is a memory BAR
    /// that firmware has assigned an address.
    pub fn memory_bar(&self, index: u8) -> Option<MemoryBar> {
        if index > 5 {
            return None;
        }
        let low = self.read(REG_BAR0 + 4 * index);
        if low & 1 != 0 {
            return None; // I/O space
        }
        let mut address = u64::from(low & !0xF);
        if (low >> 1) & 3 == 2 {
            // 64-bit BAR: the next register holds the upper half.
            if index == 5 {
                return None;
            }
            address |= u64::from(self.read(REG_BAR0 + 4 * (index + 1))) << 32;
        }
        (address != 0).then_some(MemoryBar { address })
    }

    /// Size in bytes of the region decoded by memory BAR `index`, found by
    /// the standard probe: write all ones, read back which address bits
    /// the device implements, restore.
    ///
    /// # Safety
    ///
    /// The device must not be in use while its BAR is being rewritten, so
    /// this may only be called by its driver before it starts using the
    /// device.
    pub unsafe fn memory_bar_size(&self, index: u8) -> Option<u64> {
        self.memory_bar(index)?;
        let offset = REG_BAR0 + 4 * index;
        let original = self.read(offset);
        // SAFETY: forwarded from this function's contract; the original
        // value is restored immediately.
        let probed = unsafe {
            pci_config::write(self.bus, self.device, self.function, offset, u32::MAX);
            let probed = self.read(offset);
            pci_config::write(self.bus, self.device, self.function, offset, original);
            probed
        };
        // For a 64-bit BAR only the low half is probed: regions of 4 GiB
        // or more are not supported.
        let size = u64::from(!(probed & !0xF)) + 1;
        (size > 1 && size <= u64::from(u32::MAX)).then_some(size)
    }

    /// Writes an 8-, 16- or 32-bit configuration register.
    ///
    /// # Safety
    ///
    /// The caller must be the driver for this device and write a value
    /// that is valid for the register.
    pub unsafe fn write_config(&self, offset: u8, width: u8, value: u32) {
        // SAFETY: forwarded from this function's contract.
        unsafe {
            match width {
                1 => pci_config::write8(self.bus, self.device, self.function, offset, value as u8),
                2 => {
                    pci_config::write16(self.bus, self.device, self.function, offset, value as u16)
                }
                _ => pci_config::write(self.bus, self.device, self.function, offset, value),
            }
        }
    }

    /// Iterates over the offsets of the device's capabilities, with their
    /// IDs.
    pub fn capabilities(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
        let has_list = self.read(REG_COMMAND) & STATUS_CAPABILITIES != 0;
        let mut pointer = if has_list {
            self.config_u8(REG_CAPABILITIES) & 0xFC
        } else {
            0
        };
        // A well-formed list has at most 48 entries in 256 bytes; the bound
        // protects against a malformed, circular one.
        let mut remaining = 48;
        core::iter::from_fn(move || {
            if pointer == 0 || remaining == 0 {
                return None;
            }
            remaining -= 1;
            let offset = pointer;
            let id = self.config_u8(offset);
            pointer = self.config_u8(offset + 1) & 0xFC;
            Some((offset, id))
        })
    }

    /// Sets bits in the command register (see the `COMMAND_*` constants).
    ///
    /// # Safety
    ///
    /// The caller must be the driver for this device. Enabling bus
    /// mastering lets the device write to physical memory it is told
    /// about; the driver must only give it memory it owns.
    pub unsafe fn enable(&self, command_bits: u32) {
        let value = (self.read(REG_COMMAND) & 0xFFFF) | command_bits;
        // SAFETY: forwarded from this function's contract. Only the command
        // half of the register is written with meaningful bits; the status
        // half is write-one-to-clear and is written as zero.
        unsafe { pci_config::write(self.bus, self.device, self.function, REG_COMMAND, value) };
    }
}

static DEVICES: SpinLock<Vec<PciDevice>> = SpinLock::new(Vec::new());

fn probe(bus: u8, device: u8, function: u8) -> Option<PciDevice> {
    let id = pci_config::read(bus, device, function, REG_ID);
    let vendor_id = id as u16;
    if vendor_id == VENDOR_NONE {
        return None;
    }
    let class = pci_config::read(bus, device, function, REG_CLASS);
    Some(PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id: (id >> 16) as u16,
        class: (class >> 24) as u8,
        subclass: (class >> 16) as u8,
        prog_if: (class >> 8) as u8,
    })
}

/// Scans all PCI buses and records what is found. Returns the number of
/// functions.
pub fn init() -> usize {
    let mut found = Vec::new();
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            let Some(first) = probe(bus, device, 0) else {
                continue;
            };
            found.push(first);
            let multifunction =
                pci_config::read(bus, device, 0, REG_HEADER) & HEADER_MULTIFUNCTION != 0;
            if multifunction {
                found.extend((1..8).filter_map(|function| probe(bus, device, function)));
            }
        }
    }
    let count = found.len();
    *DEVICES.lock() = found;
    count
}

/// Expansion ROM base address register (header type 0).
const REG_ROM: u8 = 0x30;
/// Address bits of a memory BAR.
const BAR_MEMORY_MASK: u32 = !0xF;
/// Address bits of the expansion ROM BAR.
const ROM_ADDRESS_MASK: u32 = !0x7FF;
/// Devices are given addresses below this: the I/O APIC, the local APIC
/// and firmware flash live from here up to 4 GiB.
const MMIO_LIMIT: u64 = 0xFEC0_0000;
/// Every assigned region gets at least a page of address space to itself,
/// so that mapping one device's registers can never expose another's.
const MIN_ALIGN: u64 = 4096;

/// Gives addresses to memory BARs that firmware left unassigned.
///
/// Firmware normally assigns every BAR, but may skip regions it considers
/// unusual (OVMF skips the 2-byte region of QEMU's pvpanic device). A
/// driver cannot use such a device until the OS does the assignment.
///
/// The method is deliberately simple: new regions go directly above the
/// highest address firmware assigned to any device on the root bus, which
/// is inside the host bridge's window (the window itself is described in
/// ACPI byte code this kernel cannot read). `is_free(start, length)` must
/// say whether a range is free of RAM and firmware-reserved memory; ranges
/// that are not, or that would reach the fixed platform devices below
/// 4 GiB, are not used. Only 32-bit addresses are assigned, only on bus 0
/// (bridges are not programmed), and I/O port BARs are left alone.
///
/// Returns the number of regions assigned. Must run before any driver
/// uses a PCI device: sizing a BAR briefly disables the device's address
/// decoding.
pub fn assign_unassigned_bars(is_free: impl Fn(u64, u64) -> bool) -> usize {
    struct Unassigned {
        device: PciDevice,
        register: u8,
        size: u64,
        wide: bool,
    }

    let devices: Vec<PciDevice> = devices().into_iter().filter(|d| d.bus == 0).collect();
    let mut top: u64 = 0;
    let mut pending = Vec::new();

    let interrupts_were_enabled = crate::arch::interrupts::save_and_disable();
    for device in &devices {
        // Bridges (header type 1) have two BARs and no ROM at 0x30.
        let bridge = (device.read(REG_HEADER) >> 16) & 0x7F == 1;
        let bar_count = if bridge { 2 } else { 6 };
        let command = device.read(REG_COMMAND);
        // SAFETY: standard BAR sizing. Decoding is switched off first so
        // that the temporary all-ones value cannot make the device respond
        // at a wrong address; every register is restored before decoding
        // is switched back on. Interrupts are off, so nothing else touches
        // the device meanwhile.
        unsafe {
            let write = |offset: u8, value: u32| {
                pci_config::write(device.bus, device.device, device.function, offset, value);
            };
            write(REG_COMMAND, command & !(COMMAND_MEMORY | COMMAND_IO));
            let mut index = 0;
            while index < bar_count {
                let register = REG_BAR0 + 4 * index;
                let original = device.read(register);
                index += 1;
                if original & 1 != 0 {
                    continue; // I/O ports
                }
                let wide = (original >> 1) & 3 == 2;
                write(register, u32::MAX);
                let probed = device.read(register) & BAR_MEMORY_MASK;
                write(register, original);
                let high = if wide && index < bar_count {
                    index += 1;
                    device.read(register + 4)
                } else {
                    0
                };
                if probed == 0 {
                    continue; // not implemented
                }
                let size = u64::from(!probed) + 1;
                let address = u64::from(original & BAR_MEMORY_MASK) | (u64::from(high) << 32);
                if address == 0 {
                    pending.push(Unassigned {
                        device: *device,
                        register,
                        size,
                        wide,
                    });
                } else if address + size <= MMIO_LIMIT {
                    top = top.max(address + size);
                }
            }
            if !bridge {
                let original = device.read(REG_ROM);
                write(REG_ROM, ROM_ADDRESS_MASK);
                let probed = device.read(REG_ROM) & ROM_ADDRESS_MASK;
                write(REG_ROM, original);
                // Firmware may leave the sizing pattern in a ROM register
                // it did not assign; only an address in the window counts.
                let address = u64::from(original & ROM_ADDRESS_MASK);
                let size = u64::from(!probed) + 1;
                if probed != 0 && address != 0 && address + size <= MMIO_LIMIT {
                    top = top.max(address + size);
                }
            }
            write(REG_COMMAND, command);
        }
    }

    let mut assigned = 0;
    // Without a single firmware-assigned region there is no way to tell
    // where the bridge's window is.
    if top != 0 {
        for region in &pending {
            let align = region.size.max(MIN_ALIGN);
            let address = top.next_multiple_of(align);
            let end = address + region.size;
            if end > MMIO_LIMIT || !is_free(address, align) {
                continue;
            }
            let device = &region.device;
            // SAFETY: the BAR is unassigned, so the device is not in use
            // and decodes nothing through it; the new address was checked
            // to be free. For a 64-bit BAR the upper half is set to zero.
            unsafe {
                let write = |offset: u8, value: u32| {
                    pci_config::write(device.bus, device.device, device.function, offset, value);
                };
                let flags = device.read(region.register) & !BAR_MEMORY_MASK;
                write(region.register, address as u32 | flags);
                if region.wide {
                    write(region.register + 4, 0);
                }
            }
            if u64::from(device.read(region.register) & BAR_MEMORY_MASK) == address {
                top = address + align;
                assigned += 1;
            }
        }
    }
    crate::arch::interrupts::restore(interrupts_were_enabled);
    assigned
}

/// All PCI functions found by [`init`].
pub fn devices() -> Vec<PciDevice> {
    DEVICES.lock().clone()
}

/// The first device with the given vendor ID and one of `device_ids`.
pub fn find(vendor_id: u16, device_ids: &[u16]) -> Option<PciDevice> {
    DEVICES
        .lock()
        .iter()
        .find(|d| d.vendor_id == vendor_id && device_ids.contains(&d.device_id))
        .copied()
}
