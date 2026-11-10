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
//! The transport and the queue are in [`super::virtio`].
//!
//! That makes the driver slow but small and easy to verify. Interrupt
//! driven completion, larger and concurrent requests, and zero-copy I/O are
//! the obvious next steps and do not change the interface above it.

use alloc::boxed::Box;

use super::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use super::registry::{self, Origin};
use super::virtio::{self, DESC_NEXT, DESC_WRITE, DmaPage, Transport, VirtioError, Virtqueue};
use crate::mm::PAGE_SIZE;
use crate::pci::{self, PciDevice};
use crate::{sched, time};

/// Modern (0x1042) and transitional (0x1001) block device IDs.
const DEVICE_IDS: [u16; 2] = [0x1042, 0x1001];

/// Queue size used by the driver. Three descriptors are needed per request
/// and only one request is ever outstanding.
const QUEUE_SIZE: u16 = 8;

const REQUEST_READ: u32 = 0;
const REQUEST_WRITE: u32 = 1;
const STATUS_OK: u8 = 0;

/// Sectors per request: one bounce page.
const MAX_SECTORS: usize = PAGE_SIZE as usize / SECTOR_SIZE;
/// How long to wait for the device to complete a request.
const TIMEOUT_TICKS: u64 = 5 * time::TICK_HZ as u64;

/// An initialised VirtIO block device.
pub struct VirtioBlk {
    sectors: u64,
    /// The request queue.
    queue: Virtqueue,
    /// Request header (16 bytes) and, at offset 16, the status byte.
    header: DmaPage,
    /// Bounce buffer for request data.
    data: DmaPage,
}

impl VirtioBlk {
    /// Brings the device at `pci` to the "driver OK" state.
    fn new(pci: &PciDevice) -> Result<Self, VirtioError> {
        // No optional features.
        let transport = Transport::begin(pci, 0)?;
        let device = Self {
            // Capacity in 512-byte sectors: the first field of the block
            // device configuration.
            sectors: u64::from(transport.config.read32(0))
                | (u64::from(transport.config.read32(4)) << 32),
            // Queue 0 is the request queue.
            queue: transport.queue(0, QUEUE_SIZE)?,
            header: DmaPage::new()?,
            data: DmaPage::new()?,
        };
        transport.start();
        Ok(device)
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
        self.queue
            .set_descriptor(0, self.header.phys, 16, DESC_NEXT, 1);
        self.queue
            .set_descriptor(1, self.data.phys, length as u32, data_flags | DESC_NEXT, 2);
        self.queue
            .set_descriptor(2, self.header.phys + 16, 1, DESC_WRITE, 0);
        self.queue.submit(0);

        // Wait for the device to hand the chain back.
        let deadline = time::ticks() + TIMEOUT_TICKS;
        while self.queue.pop_used().is_none() {
            if time::ticks() > deadline {
                return Err(BlockError::Timeout);
            }
            sched::yield_now();
        }

        // SAFETY: the status byte in the driver-owned header page, written
        // by the device before it handed the chain back.
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
    let Some(pci) = pci::find(virtio::VENDOR, &DEVICE_IDS) else {
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
