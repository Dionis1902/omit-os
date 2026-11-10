// SPDX-License-Identifier: GPL-2.0-only
//! NVMe driver.
//!
//! NVMe is the interface of current solid-state disks: a PCI device with
//! a small register block and pairs of queues in memory — the driver puts
//! 64-byte commands into a submission queue and the device answers in a
//! completion queue. Written from the NVM Express Base Specification.
//!
//! This driver is deliberately minimal, like the VirtIO one: it uses the
//! first namespace of the first controller, one I/O queue pair, one
//! command at a time, a single bounce page for data (so at most 4 KiB per
//! command), and it polls for completion instead of using interrupts.
//! Every write is followed by a flush, so data has reached the medium
//! when a write returns. Namespaces with a sector size other than 512
//! bytes are not supported.

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt;
use core::sync::atomic::{Ordering, fence};

use super::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use super::registry::{self, Origin};
use crate::arch::paging::{self, MapError};
use crate::mm::{self, PAGE_SIZE, frame};
use crate::pci::{self, COMMAND_BUS_MASTER, COMMAND_MEMORY, PciDevice};
use crate::{sched, time};

/// PCI class code of an NVMe controller: mass storage, non-volatile
/// memory, NVM Express.
const CLASS: (u8, u8, u8) = (0x01, 0x08, 0x02);

// Controller registers.
const REG_CAP: u64 = 0x00;
const REG_CC: u64 = 0x14;
const REG_CSTS: u64 = 0x1C;
const REG_AQA: u64 = 0x24;
const REG_ASQ: u64 = 0x28;
const REG_ACQ: u64 = 0x30;
const REG_DOORBELLS: u64 = 0x1000;
/// Register space mapped: the fixed registers and the doorbells of the
/// two queue pairs used, at any doorbell stride.
const REGISTER_BYTES: u64 = 0x2000;

const CC_ENABLE: u32 = 1 << 0;
/// Submission queue entries are 2^6 bytes, completion queue entries 2^4.
const CC_ENTRY_SIZES: u32 = (6 << 16) | (4 << 20);
const CSTS_READY: u32 = 1 << 0;
const CSTS_FATAL: u32 = 1 << 1;

const ADMIN_CREATE_SQ: u32 = 0x01;
const ADMIN_CREATE_CQ: u32 = 0x05;
const ADMIN_IDENTIFY: u32 = 0x06;
const IDENTIFY_NAMESPACE: u32 = 0;
const IDENTIFY_CONTROLLER: u32 = 1;
const IO_FLUSH: u32 = 0x00;
const IO_WRITE: u32 = 0x01;
const IO_READ: u32 = 0x02;

/// Entries per queue. One command is outstanding at a time, so this only
/// has to be more than one (a full queue is indistinguishable from an
/// empty one otherwise).
const QUEUE_ENTRIES: u16 = 16;
const SQ_ENTRY: usize = 64;
const CQ_ENTRY: usize = 16;
const ADMIN_QUEUE: u16 = 0;
const IO_QUEUE: u16 = 1;
/// The namespace used.
const NAMESPACE: u32 = 1;

/// Sectors per command: what fits in the bounce page.
const MAX_SECTORS: usize = PAGE_SIZE as usize / SECTOR_SIZE;
const COMMAND_TIMEOUT_TICKS: u64 = 5 * time::TICK_HZ as u64;

/// Why an NVMe controller could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvmeError {
    /// BAR 0 is not a usable memory region.
    NoRegisters,
    /// The registers could not be mapped.
    Map(MapError),
    /// No memory for the queues.
    OutOfMemory,
    /// The controller needs a memory page size larger than 4 KiB.
    PageSize,
    /// The controller did not become ready (or idle) in time.
    NotReady,
    /// The controller reported a fatal error.
    Fatal,
    /// A command timed out.
    Timeout,
    /// A command failed; the value is the status field.
    Command(u16),
    /// The controller has no namespace 1.
    NoNamespace,
    /// The namespace's sector size is not 512 bytes; the value is the
    /// binary logarithm of its size.
    SectorSize(u8),
}

impl fmt::Display for NvmeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRegisters => write!(f, "no register region"),
            Self::Map(e) => write!(f, "cannot map registers: {e}"),
            Self::OutOfMemory => write!(f, "out of memory"),
            Self::PageSize => write!(f, "controller does not support 4 KiB pages"),
            Self::NotReady => write!(f, "controller did not become ready"),
            Self::Fatal => write!(f, "controller reported a fatal error"),
            Self::Timeout => write!(f, "command timed out"),
            Self::Command(status) => write!(f, "command failed with status {status:#x}"),
            Self::NoNamespace => write!(f, "no namespace"),
            Self::SectorSize(shift) => {
                write!(f, "sector size of {} bytes is not supported", 1u64 << shift)
            }
        }
    }
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
    fn new() -> Result<Self, NvmeError> {
        let phys = frame::alloc_zeroed(0).ok_or(NvmeError::OutOfMemory)?;
        let virt = mm::phys_to_virt(phys, PAGE_SIZE).ok_or(NvmeError::OutOfMemory)?;
        Ok(Self { phys, virt })
    }
}

/// A submission queue and its completion queue.
struct QueuePair {
    submission: DmaPage,
    completion: DmaPage,
    /// Where the next command goes.
    tail: u16,
    /// Where the next completion is expected.
    head: u16,
    /// The phase bit a new completion entry carries; it flips each time
    /// the device wraps around the queue.
    phase: u16,
}

impl QueuePair {
    fn new() -> Result<Self, NvmeError> {
        Ok(Self {
            submission: DmaPage::new()?,
            completion: DmaPage::new()?,
            tail: 0,
            head: 0,
            phase: 1,
        })
    }
}

/// An initialised NVMe controller with one namespace.
pub struct Nvme {
    /// Virtual address of the register block.
    registers: u64,
    /// Bytes between doorbell registers.
    doorbell_stride: u64,
    admin: QueuePair,
    io: QueuePair,
    /// Bounce buffer for data.
    data: DmaPage,
    sectors: u64,
    next_command_id: u16,
}

impl Nvme {
    fn read32(&self, offset: u64) -> u32 {
        // SAFETY: `offset` is one of the register offsets above, inside
        // the mapped register block; device registers are read volatile.
        unsafe { ((self.registers + offset) as *const u32).read_volatile() }
    }

    fn write32(&self, offset: u64, value: u32) {
        // SAFETY: as in `read32`; the values written are those the
        // specification defines for the register.
        unsafe { ((self.registers + offset) as *mut u32).write_volatile(value) }
    }

    fn write64(&self, offset: u64, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }

    /// Waits until the ready bit has the given value.
    fn wait_ready(&self, ready: bool, timeout_ticks: u64) -> Result<(), NvmeError> {
        let deadline = time::ticks() + timeout_ticks;
        loop {
            let status = self.read32(REG_CSTS);
            if status & CSTS_FATAL != 0 {
                return Err(NvmeError::Fatal);
            }
            if (status & CSTS_READY != 0) == ready {
                return Ok(());
            }
            if time::ticks() > deadline {
                return Err(NvmeError::NotReady);
            }
            sched::yield_now();
        }
    }

    /// Brings the controller at `pci` up and finds its first namespace.
    fn new(pci: &PciDevice) -> Result<Self, NvmeError> {
        let bar = pci.memory_bar(0).ok_or(NvmeError::NoRegisters)?;
        // SAFETY: this driver is taking over the device; nothing else
        // uses it. Memory decoding is needed for the registers, bus
        // mastering for the device to read and write the queues.
        unsafe { pci.enable(COMMAND_MEMORY | COMMAND_BUS_MASTER) };
        let registers = paging::map_mmio(bar.address, REGISTER_BYTES).map_err(NvmeError::Map)?;

        let mut nvme = Self {
            registers,
            doorbell_stride: 4,
            admin: QueuePair::new()?,
            io: QueuePair::new()?,
            data: DmaPage::new()?,
            sectors: 0,
            next_command_id: 0,
        };
        let capabilities =
            u64::from(nvme.read32(REG_CAP)) | (u64::from(nvme.read32(REG_CAP + 4)) << 32);
        // Smallest page size the controller supports, as a power of two
        // above 4 KiB; the queues and the bounce buffer are 4 KiB pages.
        if (capabilities >> 48) & 0xF != 0 {
            return Err(NvmeError::PageSize);
        }
        nvme.doorbell_stride = 4 << ((capabilities >> 32) & 0xF);
        // Worst-case time to become ready, in units of 500 ms.
        let ready_ticks = ((capabilities >> 24) & 0xFF).max(1) * u64::from(time::TICK_HZ) / 2;

        // The firmware may have left the controller enabled: disable it,
        // set up the admin queues, enable it.
        nvme.write32(REG_CC, nvme.read32(REG_CC) & !CC_ENABLE);
        nvme.wait_ready(false, ready_ticks)?;
        let sizes = u32::from(QUEUE_ENTRIES - 1);
        nvme.write32(REG_AQA, (sizes << 16) | sizes);
        nvme.write64(REG_ASQ, nvme.admin.submission.phys);
        nvme.write64(REG_ACQ, nvme.admin.completion.phys);
        nvme.write32(REG_CC, CC_ENTRY_SIZES | CC_ENABLE);
        nvme.wait_ready(true, ready_ticks)?;

        // The I/O queue pair: completion queue first, since the
        // submission queue refers to it. Both are physically contiguous
        // (one page); completions are polled, so no interrupt vector.
        let queue = (sizes << 16) | u32::from(IO_QUEUE);
        let mut command = [0u32; 16];
        command[0] = ADMIN_CREATE_CQ;
        command[6] = nvme.io.completion.phys as u32;
        command[7] = (nvme.io.completion.phys >> 32) as u32;
        command[10] = queue;
        command[11] = 1;
        nvme.execute(ADMIN_QUEUE, command)?;
        let mut command = [0u32; 16];
        command[0] = ADMIN_CREATE_SQ;
        command[6] = nvme.io.submission.phys as u32;
        command[7] = (nvme.io.submission.phys >> 32) as u32;
        command[10] = queue;
        command[11] = (u32::from(IO_QUEUE) << 16) | 1;
        nvme.execute(ADMIN_QUEUE, command)?;

        // The namespace: its size and sector size.
        nvme.identify(IDENTIFY_NAMESPACE, NAMESPACE)
            .map_err(|_| NvmeError::NoNamespace)?;
        let (size, format_index) = (nvme.data_u64(0), nvme.data_u8(26) & 0x0F);
        if size == 0 {
            return Err(NvmeError::NoNamespace);
        }
        // LBA format descriptors start at byte 128; bits 16-23 hold the
        // binary logarithm of the sector size.
        let shift = nvme.data_u8(128 + 4 * usize::from(format_index) + 2);
        if shift != 9 {
            return Err(NvmeError::SectorSize(shift));
        }
        nvme.sectors = size;
        Ok(nvme)
    }

    fn data_u8(&self, offset: usize) -> u8 {
        debug_assert!(offset < PAGE_SIZE as usize);
        // SAFETY: inside the driver-owned bounce page; the device has
        // finished writing it (commands are executed one at a time).
        unsafe { self.data.virt.add(offset).read_volatile() }
    }

    fn data_u64(&self, offset: usize) -> u64 {
        (0..8).fold(0, |value, index| {
            value | (u64::from(self.data_u8(offset + index)) << (8 * index))
        })
    }

    /// The controller's model name, from its identify data.
    fn model(&mut self) -> Result<String, NvmeError> {
        self.identify(IDENTIFY_CONTROLLER, 0)?;
        let bytes: alloc::vec::Vec<u8> = (24..64).map(|offset| self.data_u8(offset)).collect();
        Ok(String::from_utf8_lossy(&bytes).trim().into())
    }

    /// Runs an identify command; the 4096 bytes of result are in the
    /// bounce page afterwards.
    fn identify(&mut self, what: u32, namespace: u32) -> Result<(), NvmeError> {
        let mut command = [0u32; 16];
        command[0] = ADMIN_IDENTIFY;
        command[1] = namespace;
        command[6] = self.data.phys as u32;
        command[7] = (self.data.phys >> 32) as u32;
        command[10] = what;
        self.execute(ADMIN_QUEUE, command)
    }

    /// Submits one command on the given queue and waits for its
    /// completion.
    fn execute(&mut self, queue_id: u16, mut command: [u32; 16]) -> Result<(), NvmeError> {
        let command_id = self.next_command_id;
        self.next_command_id = self.next_command_id.wrapping_add(1);
        command[0] |= u32::from(command_id) << 16;

        let stride = self.doorbell_stride;
        let registers = self.registers;
        let queue = if queue_id == ADMIN_QUEUE {
            &mut self.admin
        } else {
            &mut self.io
        };
        // SAFETY: the submission queue is a driver-owned page holding
        // `QUEUE_ENTRIES` entries of 64 bytes; `tail` is below that. The
        // device only reads entries after the doorbell write below.
        unsafe {
            let slot = queue
                .submission
                .virt
                .add(usize::from(queue.tail) * SQ_ENTRY)
                .cast::<u32>();
            for (index, word) in command.iter().enumerate() {
                slot.add(index).write_volatile(*word);
            }
        }
        queue.tail = (queue.tail + 1) % QUEUE_ENTRIES;
        fence(Ordering::SeqCst);
        let doorbell = |completion: u64, value: u16| {
            let offset = REG_DOORBELLS + (2 * u64::from(queue_id) + completion) * stride;
            // SAFETY: a doorbell register of a queue this driver created,
            // inside the mapped register block.
            unsafe { ((registers + offset) as *mut u32).write_volatile(u32::from(value)) };
        };
        doorbell(0, queue.tail);

        let deadline = time::ticks() + COMMAND_TIMEOUT_TICKS;
        let status = loop {
            // SAFETY: the completion queue is a driver-owned page holding
            // `QUEUE_ENTRIES` entries of 16 bytes; `head` is below that.
            // The device writes entries, hence the volatile reads.
            let entry = unsafe {
                queue
                    .completion
                    .virt
                    .add(usize::from(queue.head) * CQ_ENTRY)
                    .cast::<u16>()
            };
            // The status word, with the phase bit, first: the device may
            // be writing the entry right now, and the rest of it is only
            // meaningful once the phase bit has changed.
            // SAFETY: word 7 of the 16-byte entry located above.
            let status = unsafe { entry.add(7).read_volatile() };
            // A new entry has the phase bit the driver expects.
            if status & 1 == queue.phase {
                fence(Ordering::SeqCst);
                // SAFETY: word 6 of the same entry.
                let identifier = unsafe { entry.add(6).read_volatile() };
                queue.head += 1;
                if queue.head == QUEUE_ENTRIES {
                    queue.head = 0;
                    queue.phase ^= 1;
                }
                doorbell(1, queue.head);
                if identifier != command_id {
                    // Not possible with one command outstanding.
                    return Err(NvmeError::Fatal);
                }
                break status >> 1;
            }
            if time::ticks() > deadline {
                return Err(NvmeError::Timeout);
            }
            sched::yield_now();
        };
        if status == 0 {
            Ok(())
        } else {
            Err(NvmeError::Command(status))
        }
    }

    /// Reads or writes up to [`MAX_SECTORS`] sectors through the bounce
    /// page.
    fn transfer(&mut self, opcode: u32, sector: u64, sectors: usize) -> Result<(), BlockError> {
        let mut command = [0u32; 16];
        command[0] = opcode;
        command[1] = NAMESPACE;
        command[6] = self.data.phys as u32;
        command[7] = (self.data.phys >> 32) as u32;
        command[10] = sector as u32;
        command[11] = (sector >> 32) as u32;
        // Number of sectors, minus one.
        command[12] = sectors as u32 - 1;
        self.execute(IO_QUEUE, command).map_err(block_error)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        let mut command = [0u32; 16];
        command[0] = IO_FLUSH;
        command[1] = NAMESPACE;
        self.execute(IO_QUEUE, command).map_err(block_error)
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

fn block_error(error: NvmeError) -> BlockError {
    match error {
        NvmeError::Timeout => BlockError::Timeout,
        _ => BlockError::Io,
    }
}

impl BlockDevice for Nvme {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        self.check(sector, buffer.len())?;
        for (index, chunk) in buffer.chunks_mut(MAX_SECTORS * SECTOR_SIZE).enumerate() {
            let at = sector + (index * MAX_SECTORS) as u64;
            self.transfer(IO_READ, at, chunk.len() / SECTOR_SIZE)?;
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
            self.transfer(IO_WRITE, at, chunk.len() / SECTOR_SIZE)?;
        }
        // A disk may hold written data in a volatile cache; the block
        // layer's contract is that it is on the medium when this returns.
        self.flush()
    }
}

/// What [`init`] found.
pub struct Found {
    /// Capacity in sectors.
    pub sectors: u64,
    /// The controller's model name.
    pub model: String,
}

/// Finds the first NVMe controller, initialises it and registers its
/// first namespace as `nvme0n1`. Returns `None` if there is no
/// controller.
pub fn init() -> Result<Option<Found>, NvmeError> {
    let Some(pci) = pci::devices()
        .into_iter()
        .find(|device| (device.class, device.subclass, device.prog_if) == CLASS)
    else {
        return Ok(None);
    };
    let mut device = Nvme::new(&pci)?;
    let model = device.model()?;
    let sectors = device.sectors;
    block::register("nvme0n1", Box::new(device));
    registry::bind(
        "nvme",
        Origin::Native,
        &alloc::format!("{:02x}:{:02x}.{}", pci.bus, pci.device, pci.function),
    );
    Ok(Some(Found { sectors, model }))
}
