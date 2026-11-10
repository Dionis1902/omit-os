// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! AHCI driver: SATA disks.
//!
//! AHCI is the standard interface of SATA controllers, and the kind of
//! disk controller virtual machines offer by default. The controller has
//! up to 32 ports; a disk on a port is given commands through a list of
//! command headers in memory, each pointing to a table that holds the
//! command (a "register FIS", the SATA form of the classic ATA command
//! block) and the list of data buffers. Written from the *Serial ATA
//! AHCI* specification and the ATA command set (ACS).
//!
//! Like the other storage drivers here it is minimal: one command at a
//! time per disk, in command slot 0, through one 4 KiB bounce page, by
//! polling; a cache flush after every write. Only ATA disks with 48-bit
//! addressing and 512-byte logical sectors; no ATAPI (CD/DVD) devices,
//! no port multipliers, no hot-plug, no native command queuing.
//!
//! The controller is not reset: the firmware has set it up (it may have
//! just booted from it), so the driver only takes each port with a disk
//! over — stops it, points it at its own memory, starts it again.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{Ordering, fence};

use super::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use super::registry::{self, Origin};
use crate::arch::paging::{self, MapError};
use crate::mm::{self, PAGE_SIZE, frame};
use crate::pci::{self, COMMAND_BUS_MASTER, COMMAND_MEMORY};
use crate::{info, sched, time, warn};

/// PCI class code: mass storage, SATA, AHCI.
const CLASS: (u8, u8, u8) = (0x01, 0x06, 0x01);
/// The AHCI registers are behind BAR 5.
const REGISTER_BAR: u8 = 5;
/// Generic registers and 32 ports of 0x80 bytes from 0x100.
const REGISTER_BYTES: u64 = 0x1100;

// Controller registers.
const HBA_CAP: u64 = 0x00;
const HBA_GHC: u64 = 0x04;
const HBA_PORTS: u64 = 0x0C;
const HBA_CAP2: u64 = 0x24;
const HBA_HANDOFF: u64 = 0x28;
const CAP_64_BIT: u32 = 1 << 31;
const GHC_INTERRUPTS: u32 = 1 << 1;
const GHC_AHCI: u32 = 1 << 31;
const CAP2_HANDOFF: u32 = 1 << 0;
const HANDOFF_BIOS_OWNED: u32 = 1 << 0;
const HANDOFF_OS_OWNED: u32 = 1 << 1;

// Port registers, relative to the port's block.
const PORT_COMMAND_LIST: u64 = 0x00;
const PORT_FIS: u64 = 0x08;
const PORT_INTERRUPT_STATUS: u64 = 0x10;
const PORT_INTERRUPT_ENABLE: u64 = 0x14;
const PORT_COMMAND: u64 = 0x18;
const PORT_TASK_FILE: u64 = 0x20;
const PORT_SIGNATURE: u64 = 0x24;
const PORT_SATA_STATUS: u64 = 0x28;
const PORT_SATA_ERROR: u64 = 0x30;
const PORT_COMMAND_ISSUE: u64 = 0x38;

const COMMAND_START: u32 = 1 << 0;
const COMMAND_FIS_RECEIVE: u32 = 1 << 4;
const COMMAND_FIS_RUNNING: u32 = 1 << 14;
const COMMAND_LIST_RUNNING: u32 = 1 << 15;
const STATUS_TASK_FILE_ERROR: u32 = 1 << 30;
const TASK_FILE_ERROR: u32 = 0x01;
const TASK_FILE_DATA_REQUEST: u32 = 0x08;
const TASK_FILE_BUSY: u32 = 0x80;
/// What a port reports for an ATA disk (as opposed to an ATAPI device,
/// a port multiplier or an enclosure).
const SIGNATURE_DISK: u32 = 0x0000_0101;

const ATA_IDENTIFY: u8 = 0xEC;
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_CACHE_EXT: u8 = 0xEA;

/// Offset of the buffer list in a command table.
const TABLE_BUFFERS: usize = 0x80;
/// Sectors per command: what fits in the bounce page.
const MAX_SECTORS: usize = PAGE_SIZE as usize / SECTOR_SIZE;
const COMMAND_TIMEOUT_TICKS: u64 = 10 * time::TICK_HZ as u64;
const PORT_TIMEOUT_TICKS: u64 = time::TICK_HZ as u64;

/// Why a controller or a disk could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AhciError {
    /// BAR 5 is not a usable memory region.
    NoRegisters,
    /// The registers could not be mapped.
    Map(MapError),
    /// No memory for the driver's data structures.
    OutOfMemory,
    /// The controller cannot address the memory the driver allocated.
    Address,
    /// The port did not stop or the disk did not become ready in time.
    NotReady,
    /// A command got no answer in time.
    Timeout,
    /// The disk reported an error; the value is its task file (status
    /// in the low byte, error register above it).
    Device(u32),
    /// The disk lacks 48-bit addressing or reports no capacity.
    Unsupported,
    /// The disk's logical sectors are not 512 bytes.
    SectorSize,
}

impl fmt::Display for AhciError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRegisters => write!(f, "no register region"),
            Self::Map(e) => write!(f, "cannot map registers: {e}"),
            Self::OutOfMemory => write!(f, "out of memory"),
            Self::Address => write!(f, "controller cannot address the driver's memory"),
            Self::NotReady => write!(f, "port or disk did not become ready"),
            Self::Timeout => write!(f, "command timed out"),
            Self::Device(task_file) => {
                write!(f, "disk reported an error (task file {task_file:#x})")
            }
            Self::Unsupported => write!(f, "disk without 48-bit addressing"),
            Self::SectorSize => write!(f, "logical sector size is not 512 bytes"),
        }
    }
}

/// One zeroed page the controller may read and write, owned by the
/// driver.
struct DmaPage {
    phys: u64,
    virt: *mut u8,
}

// SAFETY: the page is exclusively owned by the driver that holds the
// `DmaPage`; the raw pointer is just its direct-map address.
unsafe impl Send for DmaPage {}

impl DmaPage {
    fn new(low_only: bool) -> Result<Self, AhciError> {
        let phys = frame::alloc_zeroed(0).ok_or(AhciError::OutOfMemory)?;
        if low_only && phys >= 1 << 32 {
            // SAFETY: the frame was just allocated and never used.
            let _ = unsafe { frame::free(phys, 0) };
            return Err(AhciError::Address);
        }
        let virt = mm::phys_to_virt(phys, PAGE_SIZE).ok_or(AhciError::OutOfMemory)?;
        Ok(Self { phys, virt })
    }

    fn write8(&self, offset: usize, value: u8) {
        debug_assert!(offset < PAGE_SIZE as usize);
        // SAFETY: inside the driver-owned page.
        unsafe { self.virt.add(offset).write_volatile(value) }
    }

    fn write32(&self, offset: usize, value: u32) {
        debug_assert!(offset + 4 <= PAGE_SIZE as usize && offset.is_multiple_of(4));
        // SAFETY: inside the driver-owned page, aligned.
        unsafe { self.virt.add(offset).cast::<u32>().write_volatile(value) }
    }

    fn read16(&self, offset: usize) -> u16 {
        debug_assert!(offset + 2 <= PAGE_SIZE as usize && offset.is_multiple_of(2));
        // SAFETY: inside the driver-owned page, aligned; the device has
        // finished writing it (one command at a time).
        unsafe { self.virt.add(offset).cast::<u16>().read_volatile() }
    }

    fn clear(&self, length: usize) {
        debug_assert!(length <= PAGE_SIZE as usize);
        // SAFETY: inside the driver-owned page.
        unsafe { core::ptr::write_bytes(self.virt, 0, length) };
    }
}

/// A SATA disk on a port.
pub struct Disk {
    /// Virtual address of the port's register block.
    port: u64,
    /// The command list; only header 0 is used.
    headers: DmaPage,
    /// The command table of header 0.
    table: DmaPage,
    /// Where the controller stores what the disk sends back; required to
    /// exist, never read.
    _received: DmaPage,
    /// Bounce buffer for data.
    data: DmaPage,
    sectors: u64,
}

/// What a command transfers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Transfer {
    None,
    /// Bytes from the disk into the bounce page.
    In(usize),
    /// Bytes from the bounce page to the disk.
    Out(usize),
}

impl Disk {
    fn read32(&self, offset: u64) -> u32 {
        // SAFETY: a register of this port, inside the mapped block.
        unsafe { ((self.port + offset) as *const u32).read_volatile() }
    }

    fn write32(&self, offset: u64, value: u32) {
        // SAFETY: as in `read32`.
        unsafe { ((self.port + offset) as *mut u32).write_volatile(value) }
    }

    fn wait(&self, ticks: u64, mut done: impl FnMut(&Self) -> bool) -> bool {
        let deadline = time::ticks() + ticks;
        while !done(self) {
            if time::ticks() > deadline {
                return false;
            }
            sched::yield_now();
        }
        true
    }

    /// Takes over the port at `port` (which has a disk attached) and
    /// identifies the disk. Returns it with its model name.
    fn new(port: u64, low_only: bool) -> Result<(Self, String), AhciError> {
        let mut disk = Self {
            port,
            headers: DmaPage::new(low_only)?,
            table: DmaPage::new(low_only)?,
            _received: DmaPage::new(low_only)?,
            data: DmaPage::new(low_only)?,
            sectors: 0,
        };
        // Stop command processing and FIS reception before changing
        // where the port's memory is.
        let command = disk.read32(PORT_COMMAND);
        disk.write32(PORT_COMMAND, command & !COMMAND_START);
        if !disk.wait(PORT_TIMEOUT_TICKS, |d| {
            d.read32(PORT_COMMAND) & COMMAND_LIST_RUNNING == 0
        }) {
            return Err(AhciError::NotReady);
        }
        let command = disk.read32(PORT_COMMAND);
        disk.write32(PORT_COMMAND, command & !COMMAND_FIS_RECEIVE);
        if !disk.wait(PORT_TIMEOUT_TICKS, |d| {
            d.read32(PORT_COMMAND) & COMMAND_FIS_RUNNING == 0
        }) {
            return Err(AhciError::NotReady);
        }
        disk.write32(PORT_COMMAND_LIST, disk.headers.phys as u32);
        disk.write32(PORT_COMMAND_LIST + 4, (disk.headers.phys >> 32) as u32);
        disk.write32(PORT_FIS, disk._received.phys as u32);
        disk.write32(PORT_FIS + 4, (disk._received.phys >> 32) as u32);
        // No interrupts; forget old errors and events (write 1 to clear).
        disk.write32(PORT_INTERRUPT_ENABLE, 0);
        disk.write32(PORT_SATA_ERROR, u32::MAX);
        disk.write32(PORT_INTERRUPT_STATUS, u32::MAX);
        let command = disk.read32(PORT_COMMAND);
        disk.write32(PORT_COMMAND, command | COMMAND_FIS_RECEIVE);
        if !disk.wait(PORT_TIMEOUT_TICKS, |d| {
            d.read32(PORT_TASK_FILE) & (TASK_FILE_BUSY | TASK_FILE_DATA_REQUEST) == 0
        }) {
            return Err(AhciError::NotReady);
        }
        let command = disk.read32(PORT_COMMAND);
        disk.write32(PORT_COMMAND, command | COMMAND_START);

        // Header 0 always points at the one command table.
        disk.headers.write32(8, disk.table.phys as u32);
        disk.headers.write32(12, (disk.table.phys >> 32) as u32);

        disk.execute(ATA_IDENTIFY, 0, 0, Transfer::In(SECTOR_SIZE))?;
        let word = |index: usize| disk.data.read16(index * 2);
        // Word 83, bit 10: 48-bit addressing.
        if word(83) & (1 << 10) == 0 {
            return Err(AhciError::Unsupported);
        }
        // Word 106 is valid if bit 14 is set and bit 15 clear; bit 12
        // then says the logical sector size is in words 117-118 (counted
        // in 16-bit words) instead of being 512 bytes.
        let sizes = word(106);
        if sizes & 0xC000 == 0x4000
            && sizes & (1 << 12) != 0
            && (u32::from(word(117)) | (u32::from(word(118)) << 16)) != 256
        {
            return Err(AhciError::SectorSize);
        }
        let sectors = (100..104)
            .rev()
            .fold(0u64, |value, index| (value << 16) | u64::from(word(index)));
        if sectors == 0 {
            return Err(AhciError::Unsupported);
        }
        // The model name: 40 characters in words 27-46, two per word
        // with the first in the high byte.
        let model: Vec<u8> = (27..47)
            .flat_map(|index| word(index).to_be_bytes())
            .collect();
        let model = String::from_utf8_lossy(&model).trim().into();
        disk.sectors = sectors;
        Ok((disk, model))
    }

    /// Runs one ATA command and waits for it to finish.
    fn execute(
        &mut self,
        command: u8,
        sector: u64,
        count: u16,
        transfer: Transfer,
    ) -> Result<(), AhciError> {
        let (bytes, write) = match transfer {
            Transfer::None => (0, false),
            Transfer::In(bytes) => (bytes, false),
            Transfer::Out(bytes) => (bytes, true),
        };
        debug_assert!(bytes <= PAGE_SIZE as usize);
        // The command: a host-to-device register FIS of five 32-bit
        // words, with the "this is a command" bit.
        self.table.clear(TABLE_BUFFERS + 16);
        let lba = sector.to_le_bytes();
        let fis = [
            0x27,
            0x80,
            command,
            0, // type, command bit, command, features
            lba[0],
            lba[1],
            lba[2],
            0x40, // address bits 0-23, LBA mode
            lba[3],
            lba[4],
            lba[5],
            0, // address bits 24-47
            count as u8,
            (count >> 8) as u8,
            0,
            0,
        ];
        for (offset, byte) in fis.into_iter().enumerate() {
            self.table.write8(offset, byte);
        }
        if bytes != 0 {
            // One buffer: its address and its length minus one.
            self.table.write32(TABLE_BUFFERS, self.data.phys as u32);
            self.table
                .write32(TABLE_BUFFERS + 4, (self.data.phys >> 32) as u32);
            self.table.write32(TABLE_BUFFERS + 12, bytes as u32 - 1);
        }
        // Header: FIS length in words, direction, number of buffers;
        // then the count of bytes transferred, which the controller
        // updates.
        let buffers = u32::from(bytes != 0);
        self.headers
            .write32(0, 5 | (u32::from(write) << 6) | (buffers << 16));
        self.headers.write32(4, 0);
        fence(Ordering::SeqCst);

        self.write32(PORT_INTERRUPT_STATUS, u32::MAX);
        self.write32(PORT_COMMAND_ISSUE, 1);
        let deadline = time::ticks() + COMMAND_TIMEOUT_TICKS;
        loop {
            if self.read32(PORT_INTERRUPT_STATUS) & STATUS_TASK_FILE_ERROR != 0 {
                return Err(AhciError::Device(self.read32(PORT_TASK_FILE)));
            }
            if self.read32(PORT_COMMAND_ISSUE) & 1 == 0 {
                break;
            }
            if time::ticks() > deadline {
                return Err(AhciError::Timeout);
            }
            sched::yield_now();
        }
        let task_file = self.read32(PORT_TASK_FILE);
        if task_file & TASK_FILE_ERROR != 0 {
            return Err(AhciError::Device(task_file));
        }
        Ok(())
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

fn block_error(error: AhciError) -> BlockError {
    match error {
        AhciError::Timeout => BlockError::Timeout,
        _ => BlockError::Io,
    }
}

impl BlockDevice for Disk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        self.check(sector, buffer.len())?;
        for (index, chunk) in buffer.chunks_mut(MAX_SECTORS * SECTOR_SIZE).enumerate() {
            let at = sector + (index * MAX_SECTORS) as u64;
            let count = (chunk.len() / SECTOR_SIZE) as u16;
            self.execute(ATA_READ_DMA_EXT, at, count, Transfer::In(chunk.len()))
                .map_err(block_error)?;
            // SAFETY: the bounce page is driver-owned and at least
            // `chunk.len()` bytes; the disk has finished writing it.
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
            let count = (chunk.len() / SECTOR_SIZE) as u16;
            // SAFETY: as in `read`; no command is running, so the
            // controller is not reading the page while it is filled.
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), self.data.virt, chunk.len()) };
            self.execute(ATA_WRITE_DMA_EXT, at, count, Transfer::Out(chunk.len()))
                .map_err(block_error)?;
        }
        // The block layer's contract: the data is on the medium when
        // this returns, not in the disk's volatile cache.
        self.execute(ATA_FLUSH_CACHE_EXT, 0, 0, Transfer::None)
            .map_err(block_error)
    }
}

/// A disk found by [`init`].
pub struct Found {
    /// The block device name: `sda`, `sdb`, ...
    pub name: String,
    /// Capacity in sectors.
    pub sectors: u64,
    /// The disk's model name.
    pub model: String,
}

/// Finds the first AHCI controller and registers the disks on its ports:
/// the disk on port 0 as `sda`, on port 1 as `sdb`, and so on. Returns
/// `None` without a controller.
pub fn init() -> Result<Option<Vec<Found>>, AhciError> {
    let Some(pci) = pci::devices()
        .into_iter()
        .find(|device| (device.class, device.subclass, device.prog_if) == CLASS)
    else {
        return Ok(None);
    };
    let bar = pci.memory_bar(REGISTER_BAR).ok_or(AhciError::NoRegisters)?;
    // SAFETY: this driver is taking over the device. Memory decoding is
    // needed for the registers, bus mastering for the controller to read
    // and write the command structures and data.
    unsafe { pci.enable(COMMAND_MEMORY | COMMAND_BUS_MASTER) };
    let registers = paging::map_mmio(bar.address, REGISTER_BYTES).map_err(AhciError::Map)?;
    let read = |offset: u64| {
        // SAFETY: a controller register inside the mapped block.
        unsafe { ((registers + offset) as *const u32).read_volatile() }
    };
    let write = |offset: u64, value: u32| {
        // SAFETY: as above.
        unsafe { ((registers + offset) as *mut u32).write_volatile(value) }
    };

    // Ask the firmware to let go, where the controller has that
    // mechanism.
    if read(HBA_CAP2) & CAP2_HANDOFF != 0 {
        write(HBA_HANDOFF, read(HBA_HANDOFF) | HANDOFF_OS_OWNED);
        let deadline = time::ticks() + PORT_TIMEOUT_TICKS;
        while read(HBA_HANDOFF) & HANDOFF_BIOS_OWNED != 0 && time::ticks() < deadline {
            sched::yield_now();
        }
    }
    // AHCI mode (not legacy IDE emulation), no interrupts.
    write(HBA_GHC, (read(HBA_GHC) | GHC_AHCI) & !GHC_INTERRUPTS);
    let low_only = read(HBA_CAP) & CAP_64_BIT == 0;
    let implemented = read(HBA_PORTS);

    let mut found = Vec::new();
    // (Ports beyond the 26th would need a second letter; no controller
    // this was written against has that many.)
    for index in (0..26u64).filter(|index| implemented & (1 << index) != 0) {
        let port = registers + 0x100 + 0x80 * index;
        let port_read = |offset: u64| {
            // SAFETY: a register of an implemented port.
            unsafe { ((port + offset) as *const u32).read_volatile() }
        };
        // A device is present with an established link (detection 3)
        // and the interface active (power state 1)...
        let status = port_read(PORT_SATA_STATUS);
        if status & 0xF != 3 || (status >> 8) & 0xF != 1 {
            continue;
        }
        // ... and it is a disk.
        if port_read(PORT_SIGNATURE) != SIGNATURE_DISK {
            info!("ahci: port {index}: not a disk (CD/DVD drives and others are not supported)");
            continue;
        }
        match Disk::new(port, low_only) {
            Ok((disk, model)) => {
                // Named by port, so that a disk keeps its name whatever
                // else is attached: port 0 is `sda`, port 2 `sdc`.
                let name = alloc::format!("sd{}", (b'a' + index as u8) as char);
                let sectors = disk.sectors;
                block::register(&name, Box::new(disk));
                found.push(Found {
                    name,
                    sectors,
                    model,
                });
            }
            Err(error) => warn!("ahci: port {index}: cannot use the disk: {error}"),
        }
    }
    // The controller is this driver's whether or not it has disks.
    registry::bind(
        "ahci",
        Origin::Native,
        &alloc::format!("{:02x}:{:02x}.{}", pci.bus, pci.device, pci.function),
    );
    Ok(Some(found))
}
