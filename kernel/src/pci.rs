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
