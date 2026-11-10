// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! What VirtIO devices have in common (VirtIO 1.x over PCI, the "modern"
//! interface): finding and mapping the register blocks, bringing a device
//! up to "driver OK", and split virtqueues.
//!
//! Queues are small (at most [`MAX_QUEUE_SIZE`] entries, so that each of
//! their three parts fits one page), completion is found by polling the
//! used ring, and the device is asked not to interrupt.

use core::fmt;
use core::sync::atomic::{Ordering, fence};

use crate::arch::paging::{self, MapError};
use crate::mm::{self, PAGE_SIZE, frame};
use crate::pci::{COMMAND_BUS_MASTER, COMMAND_MEMORY, PciDevice};

/// PCI vendor ID of all VirtIO devices.
pub const VENDOR: u16 = 0x1AF4;

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

/// The next descriptor continues the chain.
pub const DESC_NEXT: u16 = 1;
/// The device writes to the buffer (otherwise it only reads it).
pub const DESC_WRITE: u16 = 2;

/// Tells the device that the driver does not want used-buffer interrupts.
const AVAIL_NO_INTERRUPT: u16 = 1;

/// Largest queue this code sets up.
pub const MAX_QUEUE_SIZE: u16 = 128;

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
    /// The device offers no usable queue with the index needed.
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
pub struct Registers {
    base: u64,
}

impl Registers {
    // SAFETY (all accessors): `base` is a device mapping established by
    // `map_capability` covering the structure the offsets refer to, and
    // the offsets used by the drivers are within it and suitably aligned.
    // Device registers are accessed with volatile operations.
    /// Reads a byte.
    pub fn read8(self, offset: u64) -> u8 {
        // SAFETY: see above.
        unsafe { ((self.base + offset) as *const u8).read_volatile() }
    }
    /// Reads a 16-bit register.
    pub fn read16(self, offset: u64) -> u16 {
        // SAFETY: see above.
        unsafe { ((self.base + offset) as *const u16).read_volatile() }
    }
    /// Reads a 32-bit register.
    pub fn read32(self, offset: u64) -> u32 {
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
pub struct DmaPage {
    /// Physical address, which is what the device is told.
    pub phys: u64,
    /// Where the kernel sees the page.
    pub virt: *mut u8,
}

// SAFETY: the page is exclusively owned by the driver that holds the
// `DmaPage`; the raw pointer is just its direct-map address.
unsafe impl Send for DmaPage {}

impl DmaPage {
    /// Allocates a zeroed page.
    pub fn new() -> Result<Self, VirtioError> {
        let phys = frame::alloc_zeroed(0).ok_or(VirtioError::OutOfMemory)?;
        let virt = mm::phys_to_virt(phys, PAGE_SIZE).ok_or(VirtioError::OutOfMemory)?;
        Ok(Self { phys, virt })
    }
}

/// A device between reset and "driver OK".
pub struct Transport {
    common: Registers,
    notify: Registers,
    notify_multiplier: u32,
    /// The device-specific configuration structure.
    pub config: Registers,
    /// The low feature word accepted by both sides.
    pub features: u32,
    status: u8,
}

impl Transport {
    /// Resets the device at `pci` and negotiates features: VirtIO 1.0,
    /// plus those of the device's first 32 feature bits that are also set
    /// in `wanted`.
    pub fn begin(pci: &PciDevice, wanted: u32) -> Result<Self, VirtioError> {
        let (common, _) = map_capability(pci, CFG_COMMON, "common configuration")?;
        let (notify, notify_cap) = map_capability(pci, CFG_NOTIFY, "notification")?;
        let (config, _) = map_capability(pci, CFG_DEVICE, "device configuration")?;

        // SAFETY: the calling driver owns the device. Memory decoding is
        // needed to reach its registers and bus mastering for it to
        // perform DMA; the only memory it is ever told about are the
        // driver's own DMA pages.
        unsafe { pci.enable(COMMAND_MEMORY | COMMAND_BUS_MASTER) };

        // Reset, then announce a driver.
        common.write8(COMMON_STATUS, 0);
        common.write8(COMMON_STATUS, STATUS_ACKNOWLEDGE);
        common.write8(COMMON_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        common.write32(COMMON_DEVICE_FEATURE_SELECT, 1);
        if common.read32(COMMON_DEVICE_FEATURE) & FEATURE_VERSION_1_HIGH == 0 {
            return Err(VirtioError::LegacyOnly);
        }
        common.write32(COMMON_DEVICE_FEATURE_SELECT, 0);
        let features = common.read32(COMMON_DEVICE_FEATURE) & wanted;
        common.write32(COMMON_DRIVER_FEATURE_SELECT, 0);
        common.write32(COMMON_DRIVER_FEATURE, features);
        common.write32(COMMON_DRIVER_FEATURE_SELECT, 1);
        common.write32(COMMON_DRIVER_FEATURE, FEATURE_VERSION_1_HIGH);
        let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
        common.write8(COMMON_STATUS, status);
        if common.read8(COMMON_STATUS) & STATUS_FEATURES_OK == 0 {
            return Err(VirtioError::FeaturesRejected);
        }
        Ok(Self {
            common,
            notify,
            notify_multiplier: notify_cap.notify_multiplier,
            config,
            features,
            status,
        })
    }

    /// Sets up and enables queue `index` with `size` entries (a power of
    /// two, at most [`MAX_QUEUE_SIZE`]).
    pub fn queue(&self, index: u16, size: u16) -> Result<Virtqueue, VirtioError> {
        debug_assert!(size.is_power_of_two() && size <= MAX_QUEUE_SIZE);
        let common = self.common;
        common.write16(COMMON_QUEUE_SELECT, index);
        if common.read16(COMMON_QUEUE_SIZE) < size {
            return Err(VirtioError::NoQueue);
        }
        common.write16(COMMON_QUEUE_SIZE, size);
        let queue = Virtqueue {
            index,
            size,
            notify: self.notify,
            notify_offset: u64::from(common.read16(COMMON_QUEUE_NOTIFY_OFF))
                * u64::from(self.notify_multiplier),
            descriptors: DmaPage::new()?,
            available: DmaPage::new()?,
            used: DmaPage::new()?,
            available_index: 0,
            used_index: 0,
        };
        // SAFETY: the available ring is a driver-owned DMA page that
        // starts with a 16-bit flags field; the queue is not enabled yet.
        unsafe {
            queue
                .available
                .virt
                .cast::<u16>()
                .write_volatile(AVAIL_NO_INTERRUPT);
        }
        common.write64(COMMON_QUEUE_DESC, queue.descriptors.phys);
        common.write64(COMMON_QUEUE_DRIVER, queue.available.phys);
        common.write64(COMMON_QUEUE_DEVICE, queue.used.phys);
        common.write16(COMMON_QUEUE_ENABLE, 1);
        Ok(queue)
    }

    /// Tells the device that the driver is ready.
    pub fn start(self) {
        self.common
            .write8(COMMON_STATUS, self.status | STATUS_DRIVER_OK);
    }
}

/// A split virtqueue: a descriptor table, a ring of chains made available
/// to the device, and a ring of chains the device has finished with.
pub struct Virtqueue {
    index: u16,
    size: u16,
    notify: Registers,
    notify_offset: u64,
    descriptors: DmaPage,
    available: DmaPage,
    used: DmaPage,
    /// Next index to write in the available ring.
    available_index: u16,
    /// Next index to read in the used ring.
    used_index: u16,
}

impl Virtqueue {
    /// Writes descriptor `index`.
    pub fn set_descriptor(&mut self, index: u16, address: u64, length: u32, flags: u16, next: u16) {
        assert!(index < self.size);
        // SAFETY: the descriptor table is one zeroed DMA page owned by the
        // driver, holding `size` (at most 128) 16-byte descriptors;
        // `index` is below `size`. The device reads a descriptor only
        // once a chain containing it has been made available.
        unsafe {
            let entry = self.descriptors.virt.add(usize::from(index) * 16);
            entry.cast::<u64>().write_volatile(address);
            entry.add(8).cast::<u32>().write_volatile(length);
            entry.add(12).cast::<u16>().write_volatile(flags);
            entry.add(14).cast::<u16>().write_volatile(next);
        }
    }

    /// Makes the descriptor chain starting at `head` available to the
    /// device and notifies it.
    pub fn submit(&mut self, head: u16) {
        // SAFETY: the available ring is a driver-owned DMA page laid out
        // as flags (u16), index (u16), then `size` u16 entries (at most
        // 260 bytes); the slot written is within it.
        unsafe {
            let slot = usize::from(self.available_index % self.size);
            self.available
                .virt
                .add(4 + slot * 2)
                .cast::<u16>()
                .write_volatile(head);
            self.available_index = self.available_index.wrapping_add(1);
            fence(Ordering::SeqCst);
            self.available
                .virt
                .add(2)
                .cast::<u16>()
                .write_volatile(self.available_index);
        }
        fence(Ordering::SeqCst);
        self.notify.write16(self.notify_offset, self.index);
    }

    /// The next chain the device has finished with, if any: the index of
    /// its first descriptor and the number of bytes the device wrote.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        // SAFETY: the used ring is a driver-owned DMA page laid out as
        // flags (u16), index (u16), then `size` entries of two u32 (at
        // most 1028 bytes). The device writes it, hence the volatile
        // reads; it writes an entry before it advances the index.
        unsafe {
            let index = self.used.virt.add(2).cast::<u16>().read_volatile();
            if index == self.used_index {
                return None;
            }
            fence(Ordering::SeqCst);
            let slot = usize::from(self.used_index % self.size);
            let entry = self.used.virt.add(4 + slot * 8);
            let head = entry.cast::<u32>().read_volatile();
            let written = entry.add(4).cast::<u32>().read_volatile();
            self.used_index = self.used_index.wrapping_add(1);
            Some((head as u16, written))
        }
    }
}
