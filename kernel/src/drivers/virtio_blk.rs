// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! VirtIO block device driver (VirtIO 1.x over PCI, "modern" interface).
//!
//! Scope and simplifications, all deliberate for a first storage driver:
//!
//! * one request at a time, completed by polling the used ring (no
//!   interrupts yet), yielding the CPU between polls;
//! * data is copied through a single bounce page, so a request transfers
//!   at most eight sectors;
//! * only the mandatory `VIRTIO_F_VERSION_1` feature is negotiated.
//!
//! That makes the driver slow but small and easy to verify. Interrupt
//! driven completion, larger and concurrent requests, and zero-copy I/O are
//! the obvious next steps and do not change the interface above it.

use alloc::boxed::Box;
use core::fmt;
use core::sync::atomic::{Ordering, fence};

use super::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use super::registry::{self, Origin};
use crate::arch::paging::{self, MapError};
use crate::mm::{self, PAGE_SIZE, frame};
use crate::pci::{self, COMMAND_BUS_MASTER, COMMAND_MEMORY, PciDevice};
use crate::{sched, time};

const VENDOR_VIRTIO: u16 = 0x1AF4;
/// Modern (0x1042) and transitional (0x1001) block device IDs.
const DEVICE_IDS: [u16; 2] = [0x1042, 0x1001];

const CAP_VENDOR_SPECIFIC: u8 = 0x09;
const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_DEVICE: u8 = 4;

// Offsets in the common configuration structure.
const COMMON_DEVICE_FEATURE_SELECT: u64 = 0;
const COMMON_DEVICE_FEATURE: u64 = 4;
const COMMON_DRIVER_FEATURE_SELECT: u64 = 8;
const COMMON_DRIVER_FEATURE: u64 = 12;
const COMMON_STATUS: u64 = 20;
const COMMON_QUEUE_SELECT: u64 = 22;
const COMMON_QUEUE_SIZE: u64 = 24;
const COMMON_QUEUE_ENABLE: u64 = 28;
const COMMON_QUEUE_NOTIFY_OFF: u64 = 30;
const COMMON_QUEUE_DESC: u64 = 32;
const COMMON_QUEUE_DRIVER: u64 = 40;
const COMMON_QUEUE_DEVICE: u64 = 48;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

/// Feature bit 32, in the second feature word.
const FEATURE_VERSION_1_HIGH: u32 = 1 << 0;

/// Queue size used by the driver. Three descriptors are needed per request
/// and only one request is ever outstanding.
const QUEUE_SIZE: u16 = 8;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

const REQUEST_READ: u32 = 0;
const REQUEST_WRITE: u32 = 1;
const STATUS_OK: u8 = 0;

/// Sectors per request: one bounce page.
const MAX_SECTORS: usize = PAGE_SIZE as usize / SECTOR_SIZE;
/// How long to wait for the device to complete a request.
const TIMEOUT_TICKS: u64 = 5 * time::TICK_HZ as u64;

/// Why the device could not be set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    /// A required VirtIO PCI capability is missing or points at an
    /// unassigned BAR.
    MissingCapability(&'static str),
    /// The device does not implement VirtIO 1.0.
    LegacyOnly,
    /// The device rejected the negotiated features.
    FeaturesRejected,
    /// The device offers no usable request queue.
    NoQueue,
    /// Device memory could not be mapped.
    Map(MapError),
    /// Out of memory for the queue.
    OutOfMemory,
}

impl fmt::Display for VirtioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCapability(what) => write!(f, "missing {what} capability"),
            Self::LegacyOnly => write!(f, "device is not VirtIO 1.0 capable"),
            Self::FeaturesRejected => write!(f, "device rejected the feature set"),
            Self::NoQueue => write!(f, "device has no usable request queue"),
            Self::Map(e) => write!(f, "cannot map device: {e}"),
            Self::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

/// A region of device registers mapped into the kernel.
#[derive(Clone, Copy)]
struct Registers {
    base: u64,
}

impl Registers {
    // SAFETY (all accessors): `base` is a device mapping established by
    // `map_capability` covering the structure the offsets refer to, and
    // the offsets used by this driver are within it and suitably aligned.
    // Device registers are accessed with volatile operations.
    fn read8(self, offset: u64) -> u8 {
        // SAFETY: see above.
        unsafe { ((self.base + offset) as *const u8).read_volatile() }
    }
    fn read16(self, offset: u64) -> u16 {
        // SAFETY: see above.
        unsafe { ((self.base + offset) as *const u16).read_volatile() }
    }
    fn read32(self, offset: u64) -> u32 {
        // SAFETY: see above.
        unsafe { ((self.base + offset) as *const u32).read_volatile() }
    }
    fn write8(self, offset: u64, value: u8) {
        // SAFETY: see above.
        unsafe { ((self.base + offset) as *mut u8).write_volatile(value) }
    }
    fn write16(self, offset: u64, value: u16) {
        // SAFETY: see above.
        unsafe { ((self.base + offset) as *mut u16).write_volatile(value) }
    }
    fn write32(self, offset: u64, value: u32) {
        // SAFETY: see above.
        unsafe { ((self.base + offset) as *mut u32).write_volatile(value) }
    }
    fn write64(self, offset: u64, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }
}

/// A VirtIO PCI capability: where one of the device's register blocks is.
struct Capability {
    bar: u8,
    offset: u32,
    length: u32,
    /// Only for the notification capability.
    notify_multiplier: u32,
}

fn find_capability(device: &PciDevice, wanted: u8) -> Option<Capability> {
    device
        .capabilities()
        .filter(|&(_, id)| id == CAP_VENDOR_SPECIFIC)
        .find(|&(offset, _)| device.config_u8(offset + 3) == wanted)
        .map(|(offset, _)| Capability {
            bar: device.config_u8(offset + 4),
            offset: device.config_u32(offset + 8),
            length: device.config_u32(offset + 12),
            notify_multiplier: device.config_u32(offset + 16),
        })
}

fn map_capability(
    device: &PciDevice,
    kind: u8,
    name: &'static str,
) -> Result<(Registers, Capability), VirtioError> {
    let capability = find_capability(device, kind).ok_or(VirtioError::MissingCapability(name))?;
    let bar = device
        .memory_bar(capability.bar)
        .ok_or(VirtioError::MissingCapability(name))?;
    let base = paging::map_mmio(
        bar.address + u64::from(capability.offset),
        u64::from(capability.length.max(4)),
    )
    .map_err(VirtioError::Map)?;
    Ok((Registers { base }, capability))
}

/// One page of memory the device may read and write, owned by the driver.
struct DmaPage {
    phys: u64,
    virt: *mut u8,
}

// SAFETY: the page is exclusively owned by the driver that holds the
// `DmaPage`; the raw pointer is just its direct-map address.
unsafe impl Send for DmaPage {}

impl DmaPage {
    fn new() -> Result<Self, VirtioError> {
        let phys = frame::alloc_zeroed(0).ok_or(VirtioError::OutOfMemory)?;
        let virt = mm::phys_to_virt(phys, PAGE_SIZE).ok_or(VirtioError::OutOfMemory)?;
        Ok(Self { phys, virt })
    }
}

/// An initialised VirtIO block device.
pub struct VirtioBlk {
    notify: Registers,
    notify_offset: u64,
    sectors: u64,
    /// Descriptor table, available ring and used ring.
    descriptors: DmaPage,
    available: DmaPage,
    used: DmaPage,
    /// Request header (16 bytes) and, at offset 16, the status byte.
    header: DmaPage,
    /// Bounce buffer for request data.
    data: DmaPage,
    /// Next index to write in the available ring.
    available_index: u16,
    /// Next index to read in the used ring.
    used_index: u16,
}

impl VirtioBlk {
    /// Brings the device at `pci` to the "driver OK" state.
    fn new(pci: &PciDevice) -> Result<Self, VirtioError> {
        let (common, _) = map_capability(pci, CFG_COMMON, "common configuration")?;
        let (notify, notify_cap) = map_capability(pci, CFG_NOTIFY, "notification")?;
        let (config, _) = map_capability(pci, CFG_DEVICE, "device configuration")?;

        // SAFETY: this driver owns the device. Memory decoding is needed to
        // reach its registers and bus mastering for it to perform DMA; the
        // only memory it is ever told about are the driver's own DMA pages.
        unsafe { pci.enable(COMMAND_MEMORY | COMMAND_BUS_MASTER) };

        // Reset, then announce a driver.
        common.write8(COMMON_STATUS, 0);
        common.write8(COMMON_STATUS, STATUS_ACKNOWLEDGE);
        common.write8(COMMON_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        // Feature negotiation: VERSION_1 only.
        common.write32(COMMON_DEVICE_FEATURE_SELECT, 1);
        if common.read32(COMMON_DEVICE_FEATURE) & FEATURE_VERSION_1_HIGH == 0 {
            return Err(VirtioError::LegacyOnly);
        }
        common.write32(COMMON_DRIVER_FEATURE_SELECT, 0);
        common.write32(COMMON_DRIVER_FEATURE, 0);
        common.write32(COMMON_DRIVER_FEATURE_SELECT, 1);
        common.write32(COMMON_DRIVER_FEATURE, FEATURE_VERSION_1_HIGH);
        let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
        common.write8(COMMON_STATUS, status);
        if common.read8(COMMON_STATUS) & STATUS_FEATURES_OK == 0 {
            return Err(VirtioError::FeaturesRejected);
        }

        // Queue 0 is the request queue.
        common.write16(COMMON_QUEUE_SELECT, 0);
        if common.read16(COMMON_QUEUE_SIZE) < QUEUE_SIZE {
            return Err(VirtioError::NoQueue);
        }
        common.write16(COMMON_QUEUE_SIZE, QUEUE_SIZE);
        let device = Self {
            notify,
            notify_offset: u64::from(common.read16(COMMON_QUEUE_NOTIFY_OFF))
                * u64::from(notify_cap.notify_multiplier),
            // Capacity in 512-byte sectors: the first field of the block
            // device configuration.
            sectors: u64::from(config.read32(0)) | (u64::from(config.read32(4)) << 32),
            descriptors: DmaPage::new()?,
            available: DmaPage::new()?,
            used: DmaPage::new()?,
            header: DmaPage::new()?,
            data: DmaPage::new()?,
            available_index: 0,
            used_index: 0,
        };
        common.write64(COMMON_QUEUE_DESC, device.descriptors.phys);
        common.write64(COMMON_QUEUE_DRIVER, device.available.phys);
        common.write64(COMMON_QUEUE_DEVICE, device.used.phys);
        common.write16(COMMON_QUEUE_ENABLE, 1);
        common.write8(COMMON_STATUS, status | STATUS_DRIVER_OK);
        Ok(device)
    }

    /// Writes descriptor `index` of the queue.
    fn set_descriptor(&mut self, index: u16, address: u64, length: u32, flags: u16, next: u16) {
        // SAFETY: the descriptor table is one zeroed DMA page owned by the
        // driver, holding `QUEUE_SIZE` 16-byte descriptors; `index` is
        // below `QUEUE_SIZE`. The device only reads the table after being
        // notified.
        unsafe {
            let entry = self.descriptors.virt.add(usize::from(index) * 16);
            entry.cast::<u64>().write_volatile(address);
            entry.add(8).cast::<u32>().write_volatile(length);
            entry.add(12).cast::<u16>().write_volatile(flags);
            entry.add(14).cast::<u16>().write_volatile(next);
        }
    }

    /// Performs one request of at most [`MAX_SECTORS`] sectors through the
    /// bounce page.
    fn request(&mut self, kind: u32, sector: u64, length: usize) -> Result<(), BlockError> {
        debug_assert!(length <= PAGE_SIZE as usize);
        // SAFETY: `header` is a driver-owned DMA page: the 16-byte request
        // header goes at offset 0 and the status byte at offset 16. The
        // device is idle (one request at a time), so nothing else accesses
        // the page.
        unsafe {
            self.header.virt.cast::<u32>().write_volatile(kind);
            self.header.virt.add(4).cast::<u32>().write_volatile(0);
            self.header.virt.add(8).cast::<u64>().write_volatile(sector);
            self.header.virt.add(16).write_volatile(0xFF);
        }

        // Chain: header (device reads), data, status (device writes).
        let data_flags = if kind == REQUEST_READ { DESC_WRITE } else { 0 };
        self.set_descriptor(0, self.header.phys, 16, DESC_NEXT, 1);
        self.set_descriptor(1, self.data.phys, length as u32, data_flags | DESC_NEXT, 2);
        self.set_descriptor(2, self.header.phys + 16, 1, DESC_WRITE, 0);

        // Publish descriptor chain 0 in the available ring and notify.
        // SAFETY: the available ring is a driver-owned DMA page laid out as
        // flags (u16), index (u16), then `QUEUE_SIZE` u16 entries; the slot
        // written is within it.
        unsafe {
            let slot = usize::from(self.available_index % QUEUE_SIZE);
            self.available
                .virt
                .add(4 + slot * 2)
                .cast::<u16>()
                .write_volatile(0);
            self.available_index = self.available_index.wrapping_add(1);
            fence(Ordering::SeqCst);
            self.available
                .virt
                .add(2)
                .cast::<u16>()
                .write_volatile(self.available_index);
        }
        fence(Ordering::SeqCst);
        self.notify.write16(self.notify_offset, 0);

        // Wait for the device to move the used index.
        let deadline = time::ticks() + TIMEOUT_TICKS;
        loop {
            // SAFETY: the used ring is a driver-owned DMA page whose second
            // u16 is the device's index; the device writes it, hence the
            // volatile read.
            let used = unsafe { self.used.virt.add(2).cast::<u16>().read_volatile() };
            if used != self.used_index {
                self.used_index = used;
                break;
            }
            if time::ticks() > deadline {
                return Err(BlockError::Timeout);
            }
            sched::yield_now();
        }

        // SAFETY: the status byte in the driver-owned header page, written
        // by the device before it advanced the used index.
        let status = unsafe { self.header.virt.add(16).read_volatile() };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(BlockError::Io)
        }
    }

    fn check(&self, sector: u64, length: usize) -> Result<(), BlockError> {
        let count = (length / SECTOR_SIZE) as u64;
        if !length.is_multiple_of(SECTOR_SIZE)
            || sector
                .checked_add(count)
                .is_none_or(|end| end > self.sectors)
        {
            return Err(BlockError::OutOfRange);
        }
        Ok(())
    }
}

impl BlockDevice for VirtioBlk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        self.check(sector, buffer.len())?;
        for (index, chunk) in buffer.chunks_mut(MAX_SECTORS * SECTOR_SIZE).enumerate() {
            let at = sector + (index * MAX_SECTORS) as u64;
            self.request(REQUEST_READ, at, chunk.len())?;
            // SAFETY: the bounce page is driver-owned and at least
            // `chunk.len()` bytes; the device has finished writing it.
            unsafe {
                core::ptr::copy_nonoverlapping(self.data.virt, chunk.as_mut_ptr(), chunk.len())
            };
        }
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), BlockError> {
        self.check(sector, data.len())?;
        for (index, chunk) in data.chunks(MAX_SECTORS * SECTOR_SIZE).enumerate() {
            let at = sector + (index * MAX_SECTORS) as u64;
            // SAFETY: as in `read`; the device is idle, so it is not
            // reading the page while it is filled.
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), self.data.virt, chunk.len()) };
            self.request(REQUEST_WRITE, at, chunk.len())?;
        }
        Ok(())
    }
}

/// Finds the first VirtIO block device, initialises it and registers it as
/// `vda`. Returns its capacity in sectors, or `None` if there is no such
/// device.
pub fn init() -> Result<Option<u64>, VirtioError> {
    let Some(pci) = pci::find(VENDOR_VIRTIO, &DEVICE_IDS) else {
        return Ok(None);
    };
    let device = VirtioBlk::new(&pci)?;
    let sectors = device.sectors;
    block::register("vda", Box::new(device));
    registry::bind(
        "virtio-blk",
        Origin::Native,
        &alloc::format!("{:02x}:{:02x}.{}", pci.bus, pci.device, pci.function),
    );
    Ok(Some(sectors))
}
