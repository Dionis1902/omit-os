// SPDX-License-Identifier: GPL-2.0-only
//! USB keyboards and mice on an xHCI host controller.
//!
//! xHCI is the USB host controller interface of current machines (it
//! serves USB 3 and, through the same registers, USB 2 and 1 devices).
//! Written from the *eXtensible Host Controller Interface* specification
//! and the USB 2.0 and HID specifications.
//!
//! What this driver does is narrow on purpose: it brings the controller
//! up, enumerates devices plugged directly into its ports, and for each
//! keyboard or mouse that supports the HID *boot protocol* (all ordinary
//! ones do) it polls the device's interrupt endpoint and feeds the
//! reports into the same input path as the PS/2 devices.
//!
//! Limits, all deliberate for a first USB driver:
//!
//! * hubs are supported (up to five in a row), but are asked about
//!   their ports twice a second instead of being listened to;
//! * only HID boot keyboards and mice; no storage, no other class;
//! * no interrupts: one kernel thread looks at the controller's event
//!   ring every timer tick (10 ms), which adds up to that much latency;
//! * the first xHCI controller only; no power management;
//! * devices are enumerated one at a time, and a device that fails any
//!   step is skipped.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering, fence};

use omit_term::hid::{self, BootKeyboard};
use omit_term::scancode::Decoder;

use super::registry::{self, Origin};
use crate::arch::paging::{self, MapError};
use crate::mm::{self, PAGE_SIZE, frame};
use crate::pci::{self, COMMAND_BUS_MASTER, COMMAND_MEMORY, PciDevice};
use crate::{info, input, sched, time, warn};

/// PCI class code: serial bus, USB, xHCI.
const CLASS: (u8, u8, u8) = (0x0C, 0x03, 0x30);

// Capability registers.
const CAP_LENGTH: u64 = 0x00;
const CAP_HCSPARAMS1: u64 = 0x04;
const CAP_HCSPARAMS2: u64 = 0x08;
const CAP_HCCPARAMS1: u64 = 0x10;
const CAP_DBOFF: u64 = 0x14;
const CAP_RTSOFF: u64 = 0x18;

// Operational registers, relative to the operational base.
const OP_USBCMD: u64 = 0x00;
const OP_USBSTS: u64 = 0x04;
const OP_PAGESIZE: u64 = 0x08;
const OP_CRCR: u64 = 0x18;
const OP_DCBAAP: u64 = 0x30;
const OP_CONFIG: u64 = 0x38;
const OP_PORTS: u64 = 0x400;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const STS_HALTED: u32 = 1 << 0;
const STS_NOT_READY: u32 = 1 << 11;

const PORT_CONNECTED: u32 = 1 << 0;
const PORT_ENABLED: u32 = 1 << 1;
const PORT_RESET: u32 = 1 << 4;
const PORT_POWER: u32 = 1 << 9;
/// The "something changed" bits, cleared by writing 1.
const PORT_CHANGES: u32 = 0x7F << 17;
/// The bits that keep their value when written back; every other bit is
/// either read-only or acts when written as 1.
const PORT_PRESERVE: u32 = PORT_POWER | (3 << 14) | (7 << 25);

// Interrupter 0, relative to the runtime base.
const RT_ERSTSZ: u64 = 0x28;
const RT_ERSTBA: u64 = 0x30;
const RT_ERDP: u64 = 0x38;
const ERDP_BUSY: u64 = 1 << 3;

const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_EVALUATE_CONTEXT: u32 = 13;
const EVENT_TRANSFER: u32 = 32;
const EVENT_COMMAND: u32 = 33;
const EVENT_PORT: u32 = 34;

const TRB_CYCLE: u32 = 1 << 0;
const TRB_TOGGLE_CYCLE: u32 = 1 << 1;
const TRB_INTERRUPT_ON_SHORT: u32 = 1 << 2;
const TRB_INTERRUPT: u32 = 1 << 5;
const TRB_IMMEDIATE: u32 = 1 << 6;
const TRB_DIRECTION_IN: u32 = 1 << 16;

const COMPLETION_SUCCESS: u32 = 1;
const COMPLETION_SHORT_PACKET: u32 = 13;

/// TRBs per ring page; the last one of a transfer or command ring links
/// back to the first.
const RING_TRBS: usize = PAGE_SIZE as usize / 16;

const SPEED_FULL: u32 = 1;
const SPEED_LOW: u32 = 2;
const SPEED_HIGH: u32 = 3;

const ENDPOINT_CONTROL: u32 = 4;
const ENDPOINT_INTERRUPT_IN: u32 = 7;

const TIMEOUT_TICKS: u64 = 2 * time::TICK_HZ as u64;
/// Device slots used at most.
const MAX_SLOTS: u32 = 16;
/// Reports queued per interrupt endpoint.
const REPORT_BUFFERS: usize = 8;
const REPORT_BUFFER_SIZE: usize = 64;

/// How often hubs are asked about their ports.
const HUB_SCAN_TICKS: u64 = time::TICK_HZ as u64 / 2;

/// Auto-repeat: a held key repeats after this many ticks, then at this
/// interval.
const REPEAT_DELAY_TICKS: u64 = time::TICK_HZ as u64 / 2;
const REPEAT_INTERVAL_TICKS: u64 = time::TICK_HZ as u64 / 25;

/// Why the controller or a device could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciError {
    /// BAR 0 is not a usable memory region.
    NoRegisters,
    /// The registers could not be mapped.
    Map(MapError),
    /// No memory for the driver's data structures.
    OutOfMemory,
    /// The controller needs memory pages larger than 4 KiB.
    PageSize,
    /// The controller cannot address the memory the driver allocated
    /// (a 32-bit controller and memory above 4 GiB).
    Address,
    /// The controller did not finish a reset, start or stop in time.
    NotReady,
    /// A command or transfer got no answer in time.
    Timeout,
    /// A command or transfer failed; the value is the completion code.
    Failed(u32),
    /// The device sent something that makes no sense.
    Protocol,
}

impl fmt::Display for XhciError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRegisters => write!(f, "no register region"),
            Self::Map(e) => write!(f, "cannot map registers: {e}"),
            Self::OutOfMemory => write!(f, "out of memory"),
            Self::PageSize => write!(f, "controller does not support 4 KiB pages"),
            Self::Address => write!(f, "controller cannot address the driver's memory"),
            Self::NotReady => write!(f, "controller did not become ready"),
            Self::Timeout => write!(f, "no answer in time"),
            Self::Failed(code) => write!(f, "failed with completion code {code}"),
            Self::Protocol => write!(f, "unexpected data from the device"),
        }
    }
}

/// One zeroed page the controller may read and write, owned by the
/// driver. Pages are never freed: the controller keeps pointers to them
/// for as long as it runs, and it is never shut down.
#[derive(Clone, Copy)]
struct DmaPage {
    phys: u64,
    virt: *mut u8,
}

/// Set by the driver's thread once the devices present at boot have been
/// looked at.
static INITIAL_SCAN_DONE: AtomicBool = AtomicBool::new(false);
/// How long the boot waits for that.
const INITIAL_SCAN_TICKS: u64 = 3 * time::TICK_HZ as u64;

/// Set if the controller only takes 32-bit addresses.
static LOW_ADDRESSES_ONLY: AtomicBool = AtomicBool::new(false);

impl DmaPage {
    fn new() -> Result<Self, XhciError> {
        let phys = frame::alloc_zeroed(0).ok_or(XhciError::OutOfMemory)?;
        if phys >= 1 << 32 && LOW_ADDRESSES_ONLY.load(Ordering::Relaxed) {
            // SAFETY: the frame was just allocated and never used.
            let _ = unsafe { frame::free(phys, 0) };
            return Err(XhciError::Address);
        }
        let virt = mm::phys_to_virt(phys, PAGE_SIZE).ok_or(XhciError::OutOfMemory)?;
        Ok(Self { phys, virt })
    }

    fn read32(&self, offset: usize) -> u32 {
        debug_assert!(offset + 4 <= PAGE_SIZE as usize && offset.is_multiple_of(4));
        // SAFETY: inside the driver-owned page, aligned; the controller
        // may write the page, hence volatile.
        unsafe { self.virt.add(offset).cast::<u32>().read_volatile() }
    }

    fn write32(&self, offset: usize, value: u32) {
        debug_assert!(offset + 4 <= PAGE_SIZE as usize && offset.is_multiple_of(4));
        // SAFETY: as in `read32`.
        unsafe { self.virt.add(offset).cast::<u32>().write_volatile(value) }
    }

    fn write64(&self, offset: usize, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }

    fn bytes(&self, offset: usize, buffer: &mut [u8]) {
        debug_assert!(offset + buffer.len() <= PAGE_SIZE as usize);
        for (index, byte) in buffer.iter_mut().enumerate() {
            // SAFETY: inside the driver-owned page.
            *byte = unsafe { self.virt.add(offset + index).read_volatile() };
        }
    }

    fn clear(&self) {
        // SAFETY: the whole page is owned by the driver.
        unsafe { core::ptr::write_bytes(self.virt, 0, PAGE_SIZE as usize) };
    }
}

/// A ring the driver produces into: the command ring or a transfer ring.
struct Ring {
    page: DmaPage,
    enqueue: usize,
    cycle: u32,
}

impl Ring {
    fn new() -> Result<Self, XhciError> {
        Ok(Self {
            page: DmaPage::new()?,
            enqueue: 0,
            cycle: TRB_CYCLE,
        })
    }

    /// Appends a TRB (the cycle bit is filled in here) and returns its
    /// physical address.
    fn push(&mut self, parameter: u64, status: u32, control: u32) -> u64 {
        let offset = self.enqueue * 16;
        self.page.write64(offset, parameter);
        self.page.write32(offset + 8, status);
        // The control word last: its cycle bit hands the TRB over.
        fence(Ordering::SeqCst);
        self.page
            .write32(offset + 12, (control & !TRB_CYCLE) | self.cycle);
        let address = self.page.phys + offset as u64;
        self.enqueue += 1;
        if self.enqueue == RING_TRBS - 1 {
            // The link TRB at the end sends the controller back to the
            // start and flips the cycle bit it expects.
            let link = (RING_TRBS - 1) * 16;
            self.page.write64(link, self.page.phys);
            self.page.write32(link + 8, 0);
            self.page
                .write32(link + 12, (TRB_LINK << 10) | TRB_TOGGLE_CYCLE | self.cycle);
            self.enqueue = 0;
            self.cycle ^= TRB_CYCLE;
        }
        address
    }
}

/// An event from the controller.
#[derive(Clone, Copy)]
struct Event {
    parameter: u64,
    status: u32,
    control: u32,
}

impl Event {
    fn kind(&self) -> u32 {
        (self.control >> 10) & 0x3F
    }

    fn completion(&self) -> u32 {
        self.status >> 24
    }

    fn slot(&self) -> u8 {
        (self.control >> 24) as u8
    }

    fn endpoint(&self) -> u8 {
        ((self.control >> 16) & 0x1F) as u8
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Keyboard,
    Mouse,
}

/// An interrupt endpoint being polled.
struct Endpoint {
    slot: u8,
    /// Device context index: twice the endpoint number, plus one for IN.
    index: u8,
    kind: Kind,
    ring: Ring,
    /// Holds the report buffers.
    buffers: DmaPage,
    /// Which buffer the next queued transfer uses.
    next_buffer: usize,
    report_size: usize,
    keyboard: BootKeyboard,
    decoder: Decoder,
    /// When the held key repeats next, if one is held.
    repeat_at: Option<u64>,
}

/// Where a device is plugged in.
#[derive(Clone, Copy)]
struct Place {
    /// The controller's own port the device hangs off, directly or
    /// through hubs.
    root_port: u8,
    /// The path through hubs: four bits per hub, the nearest to the
    /// controller lowest, each the number of the hub's port.
    route: u32,
    /// How many hubs are on the way.
    tier: u8,
    speed: u32,
    /// The hub (its slot) and the port of it the device is plugged into.
    parent: Option<(u8, u8)>,
    /// For a low- or full-speed device behind a high-speed hub: that hub
    /// and its port, which translate between the speeds.
    translator: Option<(u8, u8)>,
}

impl Place {
    /// The first three words of a slot context for a device here.
    fn slot_words(&self, context_entries: u32, hub_ports: Option<u8>) -> [u32; 3] {
        let hub = u32::from(hub_ports.is_some()) << 26;
        let (translator_slot, translator_port) = self.translator.unwrap_or((0, 0));
        [
            self.route | (self.speed << 20) | hub | (context_entries << 27),
            (u32::from(self.root_port) << 16) | (u32::from(hub_ports.unwrap_or(0)) << 24),
            u32::from(translator_slot) | (u32::from(translator_port) << 8),
        ]
    }
}

impl fmt::Display for Place {
    /// `5` for the controller's port 5, `5.2` for port 2 of a hub there.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.root_port)?;
        for tier in 0..self.tier {
            write!(f, ".{}", (self.route >> (4 * tier)) & 0xF)?;
        }
        Ok(())
    }
}

/// What the driver keeps for a hub.
struct Hub {
    ports: u8,
    /// The hub's control endpoint, for asking about its ports.
    ring: Ring,
    scratch: DmaPage,
    /// Ports whose device was looked at (and perhaps found unusable), so
    /// that it is not tried again on every poll.
    tried: u16,
}

/// A device that has been set up.
struct Device {
    slot: u8,
    place: Place,
    hub: Option<Hub>,
}

/// The controller.
pub struct Xhci {
    registers: u64,
    operational: u64,
    runtime: u64,
    doorbells: u64,
    ports: u8,
    /// Bytes per context: 32 or 64.
    context_size: usize,
    device_contexts: DmaPage,
    command_ring: Ring,
    event_ring: DmaPage,
    event_dequeue: usize,
    event_cycle: u32,
    devices: Vec<Device>,
    endpoints: Vec<Endpoint>,
    /// Ports that reported a change while the driver was busy with
    /// another device; looked at by the main loop.
    changed_ports: Vec<u8>,
}

// SAFETY: the raw pointers are direct-map addresses of pages the driver
// owns; the structure is used by one thread.
unsafe impl Send for Xhci {}

/// What a control transfer carries besides the setup packet.
enum Data<'a> {
    None,
    In(&'a mut [u8]),
}

impl Xhci {
    fn read32(&self, address: u64) -> u32 {
        // SAFETY: `address` is a register inside the mapped block, built
        // from the offsets the controller reports.
        unsafe { (address as *const u32).read_volatile() }
    }

    fn write32(&self, address: u64, value: u32) {
        // SAFETY: as in `read32`.
        unsafe { (address as *mut u32).write_volatile(value) }
    }

    fn write64(&self, address: u64, value: u64) {
        self.write32(address, value as u32);
        self.write32(address + 4, (value >> 32) as u32);
    }

    fn wait(&self, mut done: impl FnMut(&Self) -> bool) -> Result<(), XhciError> {
        let deadline = time::ticks() + TIMEOUT_TICKS;
        while !done(self) {
            if time::ticks() > deadline {
                return Err(XhciError::NotReady);
            }
            sched::yield_now();
        }
        Ok(())
    }

    fn port_register(&self, port: u8) -> u64 {
        self.operational + OP_PORTS + 0x10 * u64::from(port - 1)
    }

    /// Takes the controller over from the firmware if it holds it (the
    /// "USB legacy support" capability), so that the firmware's own
    /// keyboard emulation stops touching the controller.
    fn take_from_firmware(&self, extended: u64) {
        let mut offset = extended;
        // Bounded: a damaged list must not hang the boot.
        for _ in 0..64 {
            if offset == 0 || offset + 8 > MAPPED_BYTES {
                return;
            }
            let address = self.registers + offset;
            let header = self.read32(address);
            if header & 0xFF == 1 {
                const BIOS_OWNED: u32 = 1 << 16;
                const OS_OWNED: u32 = 1 << 24;
                self.write32(address, header | OS_OWNED);
                if self
                    .wait(|xhci| xhci.read32(address) & BIOS_OWNED == 0)
                    .is_err()
                {
                    warn!("xhci: firmware did not release the controller; taking it anyway");
                    self.write32(address, (header | OS_OWNED) & !BIOS_OWNED);
                }
                // No system-management interrupts for USB events.
                let control = self.read32(address + 4);
                self.write32(address + 4, (control & 0x1FFF_FFE0) | 0xE000_0000);
                return;
            }
            let next = u64::from((header >> 8) & 0xFF) * 4;
            if next == 0 {
                return;
            }
            offset += next;
        }
    }

    /// Brings the controller at `pci` up to the point where ports can be
    /// used.
    fn new(pci: &PciDevice) -> Result<Self, XhciError> {
        let bar = pci.memory_bar(0).ok_or(XhciError::NoRegisters)?;
        // SAFETY: this driver is taking over the device. Memory decoding
        // is needed for the registers, bus mastering for the controller
        // to read and write the rings and contexts.
        unsafe { pci.enable(COMMAND_MEMORY | COMMAND_BUS_MASTER) };
        let registers = paging::map_mmio(bar.address, MAPPED_BYTES).map_err(XhciError::Map)?;

        let mut xhci = Self {
            registers,
            operational: registers,
            runtime: registers,
            doorbells: registers,
            ports: 0,
            context_size: 32,
            device_contexts: DmaPage::new()?,
            command_ring: Ring::new()?,
            event_ring: DmaPage::new()?,
            event_dequeue: 0,
            event_cycle: TRB_CYCLE,
            devices: Vec::new(),
            endpoints: Vec::new(),
            changed_ports: Vec::new(),
        };
        let capability_length = u64::from(xhci.read32(registers + CAP_LENGTH) & 0xFF);
        let structural1 = xhci.read32(registers + CAP_HCSPARAMS1);
        let structural2 = xhci.read32(registers + CAP_HCSPARAMS2);
        let capabilities = xhci.read32(registers + CAP_HCCPARAMS1);
        let doorbell_offset = u64::from(xhci.read32(registers + CAP_DBOFF) & !0x3);
        let runtime_offset = u64::from(xhci.read32(registers + CAP_RTSOFF) & !0x1F);
        xhci.ports = (structural1 >> 24) as u8;
        let last_port = capability_length + OP_PORTS + 0x10 * u64::from(xhci.ports);
        let slots = (structural1 & 0xFF).min(MAX_SLOTS);
        if capability_length < 0x20
            || last_port > MAPPED_BYTES
            || runtime_offset + 0x40 > MAPPED_BYTES
            || doorbell_offset + 4 * (u64::from(slots) + 1) > MAPPED_BYTES
        {
            return Err(XhciError::NoRegisters);
        }
        xhci.operational = registers + capability_length;
        xhci.runtime = registers + runtime_offset;
        xhci.doorbells = registers + doorbell_offset;
        xhci.context_size = if capabilities & (1 << 2) != 0 { 64 } else { 32 };
        let addresses_64_bit = capabilities & 1 != 0;

        xhci.take_from_firmware(u64::from(capabilities >> 16) * 4);

        // Stop, reset, and wait until the controller is ready again.
        let command = xhci.read32(xhci.operational + OP_USBCMD);
        xhci.write32(xhci.operational + OP_USBCMD, command & !CMD_RUN);
        xhci.wait(|x| x.read32(x.operational + OP_USBSTS) & STS_HALTED != 0)?;
        xhci.write32(xhci.operational + OP_USBCMD, CMD_RESET);
        xhci.wait(|x| {
            x.read32(x.operational + OP_USBCMD) & CMD_RESET == 0
                && x.read32(x.operational + OP_USBSTS) & STS_NOT_READY == 0
        })?;
        if xhci.read32(xhci.operational + OP_PAGESIZE) & 1 == 0 {
            return Err(XhciError::PageSize);
        }
        xhci.write32(xhci.operational + OP_CONFIG, slots);

        // Scratchpad pages, if the controller wants any: an array of
        // pointers to pages for its private use, in entry 0 of the
        // device context array.
        let scratchpads = (((structural2 >> 21) & 0x1F) << 5) | ((structural2 >> 27) & 0x1F);
        if scratchpads as usize > PAGE_SIZE as usize / 8 {
            return Err(XhciError::OutOfMemory);
        }
        if !addresses_64_bit {
            LOW_ADDRESSES_ONLY.store(true, Ordering::Relaxed);
            let early = [
                xhci.device_contexts,
                xhci.command_ring.page,
                xhci.event_ring,
            ];
            if early.iter().any(|page| page.phys >= 1 << 32) {
                return Err(XhciError::Address);
            }
        }
        if scratchpads != 0 {
            let array = DmaPage::new()?;
            for index in 0..scratchpads as usize {
                array.write64(index * 8, DmaPage::new()?.phys);
            }
            xhci.device_contexts.write64(0, array.phys);
        }
        let event_table = DmaPage::new()?;

        xhci.write64(xhci.operational + OP_DCBAAP, xhci.device_contexts.phys);
        // Command ring, with the cycle bit the first command will carry.
        xhci.write64(
            xhci.operational + OP_CRCR,
            xhci.command_ring.page.phys | u64::from(TRB_CYCLE),
        );
        // Event ring: one segment, described by a one-entry table.
        event_table.write64(0, xhci.event_ring.phys);
        event_table.write32(8, RING_TRBS as u32);
        xhci.write32(xhci.runtime + RT_ERSTSZ, 1);
        xhci.write64(xhci.runtime + RT_ERDP, xhci.event_ring.phys);
        xhci.write64(xhci.runtime + RT_ERSTBA, event_table.phys);

        // Run, without interrupts: the event ring is polled.
        xhci.write32(xhci.operational + OP_USBCMD, CMD_RUN);
        xhci.wait(|x| x.read32(x.operational + OP_USBSTS) & STS_HALTED == 0)?;
        Ok(xhci)
    }

    /// The next event on the event ring, if there is one.
    fn next_event(&mut self) -> Option<Event> {
        let offset = self.event_dequeue * 16;
        // The control word first: until its cycle bit matches, the rest
        // of the entry may still be changing.
        let control = self.event_ring.read32(offset + 12);
        if control & TRB_CYCLE != self.event_cycle {
            return None;
        }
        fence(Ordering::SeqCst);
        let event = Event {
            parameter: u64::from(self.event_ring.read32(offset))
                | (u64::from(self.event_ring.read32(offset + 4)) << 32),
            status: self.event_ring.read32(offset + 8),
            control,
        };
        self.event_dequeue += 1;
        if self.event_dequeue == RING_TRBS {
            self.event_dequeue = 0;
            self.event_cycle ^= TRB_CYCLE;
        }
        // Tell the controller how far the driver has read.
        self.write64(
            self.runtime + RT_ERDP,
            (self.event_ring.phys + (self.event_dequeue * 16) as u64) | ERDP_BUSY,
        );
        Some(event)
    }

    fn ring_doorbell(&self, slot: u8, target: u32) {
        self.write32(self.doorbells + 4 * u64::from(slot), target);
    }

    /// Waits for the event that answers the TRB at `address` (a command
    /// or the last TRB of a control transfer). Other events that arrive
    /// meanwhile are handled as usual.
    fn wait_for(&mut self, kind: u32, slot: u8, address: u64) -> Result<Event, XhciError> {
        let deadline = time::ticks() + TIMEOUT_TICKS;
        loop {
            while let Some(event) = self.next_event() {
                if event.kind() == kind && event.parameter == address {
                    return match event.completion() {
                        COMPLETION_SUCCESS | COMPLETION_SHORT_PACKET => Ok(event),
                        code => Err(XhciError::Failed(code)),
                    };
                }
                // A failed stage of the awaited control transfer.
                if kind == EVENT_TRANSFER
                    && event.kind() == EVENT_TRANSFER
                    && event.slot() == slot
                    && event.endpoint() == 1
                    && !matches!(
                        event.completion(),
                        COMPLETION_SUCCESS | COMPLETION_SHORT_PACKET
                    )
                {
                    return Err(XhciError::Failed(event.completion()));
                }
                self.handle(event);
            }
            if time::ticks() > deadline {
                return Err(XhciError::Timeout);
            }
            sched::yield_now();
        }
    }

    /// Runs a command and returns its completion event.
    fn command(&mut self, parameter: u64, control: u32) -> Result<Event, XhciError> {
        let address = self.command_ring.push(parameter, 0, control);
        self.ring_doorbell(0, 0);
        self.wait_for(EVENT_COMMAND, 0, address)
    }

    /// Offset of context `index` (0 is the slot context, 1 endpoint 0,
    /// and so on) in an input context page, whose first context is the
    /// input control context.
    fn input_context(&self, index: usize) -> usize {
        (index + 1) * self.context_size
    }

    /// A control transfer on the default endpoint of the device in
    /// `slot`. Returns the number of bytes received.
    fn control(
        &mut self,
        slot: u8,
        ring: &mut Ring,
        scratch: DmaPage,
        setup: [u8; 8],
        data: Data<'_>,
    ) -> Result<usize, XhciError> {
        let length = match &data {
            Data::None => 0,
            Data::In(buffer) => buffer.len().min(PAGE_SIZE as usize),
        };
        // Transfer type: 0 is "no data stage", 3 "data from the device".
        let transfer_type = if length == 0 { 0 } else { 3 };
        ring.push(
            u64::from_le_bytes(setup),
            8,
            (TRB_SETUP << 10) | TRB_IMMEDIATE | (transfer_type << 16),
        );
        if length != 0 {
            ring.push(
                scratch.phys,
                length as u32,
                (TRB_DATA << 10) | TRB_DIRECTION_IN,
            );
        }
        // The status stage goes the other way than the data.
        let status_direction = if length == 0 { TRB_DIRECTION_IN } else { 0 };
        let status = ring.push(0, 0, (TRB_STATUS << 10) | TRB_INTERRUPT | status_direction);
        self.ring_doorbell(slot, 1);
        self.wait_for(EVENT_TRANSFER, slot, status)?;
        if let Data::In(buffer) = data {
            scratch.bytes(0, &mut buffer[..length]);
        }
        Ok(length)
    }

    /// Sets up the device on `port`, if there is one and it is a
    /// keyboard or mouse. Returns a description for the log.
    fn enumerate(&mut self, port: u8) -> Result<Option<String>, XhciError> {
        let register = self.port_register(port);
        let status = self.read32(register);
        if status & PORT_CONNECTED == 0 {
            return Ok(None);
        }
        // USB 3 ports enable themselves; USB 2 ports need a reset.
        if status & PORT_ENABLED == 0 {
            self.write32(register, (status & PORT_PRESERVE) | PORT_RESET);
            self.wait(|x| {
                let now = x.read32(register);
                now & PORT_RESET == 0 && now & PORT_ENABLED != 0
            })?;
        }
        let status = self.read32(register);
        self.write32(register, (status & PORT_PRESERVE) | PORT_CHANGES);
        let speed = (status >> 10) & 0xF;

        self.enumerate_at(Place {
            root_port: port,
            route: 0,
            tier: 0,
            speed,
            parent: None,
            translator: None,
        })
    }

    /// Sets up the device at `place`, whose port has been reset.
    fn enumerate_at(&mut self, place: Place) -> Result<Option<String>, XhciError> {
        let slot = self.command(0, TRB_ENABLE_SLOT << 10)?.slot();
        if slot == 0 || u32::from(slot) > MAX_SLOTS {
            return Err(XhciError::Protocol);
        }
        let result = self.setup_device(slot, place);
        if !matches!(result, Ok(Some(_))) {
            // Give the slot back; the device is left alone.
            self.detach(slot);
        }
        result
    }

    /// Forgets the device in `slot` and, if it is a hub, everything
    /// plugged into it.
    fn detach(&mut self, slot: u8) {
        let children: Vec<u8> = self
            .devices
            .iter()
            .filter(|device| device.place.parent.is_some_and(|(hub, _)| hub == slot))
            .map(|device| device.slot)
            .collect();
        for child in children {
            self.detach(child);
        }
        self.devices.retain(|device| device.slot != slot);
        self.endpoints.retain(|endpoint| endpoint.slot != slot);
        let _ = self.command(0, (TRB_DISABLE_SLOT << 10) | (u32::from(slot) << 24));
        self.device_contexts.write64(usize::from(slot) * 8, 0);
    }

    /// Finishes setting up a hub: tells the controller it is one, powers
    /// its ports and looks at what is plugged into them.
    #[allow(
        clippy::too_many_arguments,
        reason = "the pieces of a device set-up in progress"
    )]
    fn setup_hub(
        &mut self,
        slot: u8,
        place: Place,
        mut ring: Ring,
        scratch: DmaPage,
        input: DmaPage,
        configuration: u8,
    ) -> Result<Option<String>, XhciError> {
        if place.tier >= 5 {
            // The route to a device has room for five hubs.
            return Ok(None);
        }
        self.control(
            slot,
            &mut ring,
            scratch,
            [0x00, 9, configuration, 0, 0, 0, 0, 0],
            Data::None,
        )?;
        // The hub descriptor (class request, type 0x29): byte 2 is the
        // number of ports, byte 5 the time from power-on to usable, in
        // units of 2 ms.
        let mut descriptor = [0u8; 8];
        self.control(
            slot,
            &mut ring,
            scratch,
            [0xA0, 6, 0, 0x29, 0, 0, 8, 0],
            Data::In(&mut descriptor),
        )?;
        // A route names ports with four bits.
        let ports = descriptor[2].min(15);
        input.clear();
        input.write32(4, 1);
        for (index, word) in place.slot_words(1, Some(ports)).into_iter().enumerate() {
            input.write32(self.input_context(0) + 4 * index, word);
        }
        self.command(
            input.phys,
            (TRB_CONFIGURE_ENDPOINT << 10) | (u32::from(slot) << 24),
        )?;
        for port in 1..=ports {
            // SET_FEATURE(PORT_POWER).
            self.control(
                slot,
                &mut ring,
                scratch,
                [0x23, 3, 8, 0, port, 0, 0, 0],
                Data::None,
            )?;
        }
        sched::sleep_ticks((u64::from(descriptor[5]) * 2).div_ceil(10).max(10));
        self.devices.push(Device {
            slot,
            place,
            hub: Some(Hub {
                ports,
                ring,
                scratch,
                tried: 0,
            }),
        });
        self.scan_hub(slot);
        Ok(Some(alloc::format!("hub with {ports} ports")))
    }

    /// Looks at the ports of the hub in `slot`: sets up devices that
    /// have appeared, forgets those that are gone.
    fn scan_hub(&mut self, slot: u8) {
        let Some(position) = self.devices.iter().position(|device| device.slot == slot) else {
            return;
        };
        // Taken out of the list while its control endpoint is in use.
        let mut device = self.devices.swap_remove(position);
        let place = device.place;
        let mut arrived = Vec::new();
        let mut gone = Vec::new();
        if let Some(hub) = &mut device.hub {
            for port in 1..=hub.ports {
                let mut status = [0u8; 4];
                let ask = |xhci: &mut Self, hub: &mut Hub, status: &mut [u8; 4]| {
                    // GET_STATUS for the port: status bits, change bits.
                    xhci.control(
                        slot,
                        &mut hub.ring,
                        hub.scratch,
                        [0xA3, 0, 0, 0, port, 0, 4, 0],
                        Data::In(status),
                    )
                    .is_ok()
                };
                if !ask(self, hub, &mut status) {
                    continue;
                }
                let connected = status[0] & 0x01 != 0;
                let bit = 1u16 << port;
                if !connected {
                    hub.tried &= !bit;
                    gone.push(port);
                    continue;
                }
                if hub.tried & bit != 0 {
                    continue;
                }
                hub.tried |= bit;
                // Reset the port (SET_FEATURE 4) and wait for it to be
                // enabled; then clear the "reset" and "connection"
                // change bits (CLEAR_FEATURE 20 and 16).
                let request = |xhci: &mut Self, hub: &mut Hub, kind: u8, feature: u8| {
                    xhci.control(
                        slot,
                        &mut hub.ring,
                        hub.scratch,
                        [0x23, kind, feature, 0, port, 0, 0, 0],
                        Data::None,
                    )
                    .is_ok()
                };
                if !request(self, hub, 3, 4) {
                    continue;
                }
                let mut enabled = false;
                for _ in 0..50 {
                    sched::sleep_ticks(1);
                    if ask(self, hub, &mut status) && status[0] & 0x10 == 0 && status[0] & 0x02 != 0
                    {
                        enabled = true;
                        break;
                    }
                }
                request(self, hub, 1, 20);
                request(self, hub, 1, 16);
                if !enabled {
                    continue;
                }
                // Bit 9 of the status: low speed; bit 10: high speed.
                let speed = match status[1] & 0x06 {
                    0x02 => SPEED_LOW,
                    0x04 => SPEED_HIGH,
                    _ => SPEED_FULL,
                };
                let translator = match (speed, place.speed) {
                    (SPEED_HIGH, _) => None,
                    (_, SPEED_HIGH) => Some((slot, port)),
                    _ => place.translator,
                };
                arrived.push(Place {
                    root_port: place.root_port,
                    route: place.route | (u32::from(port) << (4 * place.tier)),
                    tier: place.tier + 1,
                    speed,
                    parent: Some((slot, port)),
                    translator,
                });
            }
        }
        self.devices.push(device);
        for port in gone {
            let child = self
                .devices
                .iter()
                .find(|device| device.place.parent == Some((slot, port)))
                .map(|device| (device.slot, device.place));
            if let Some((child, place)) = child {
                self.detach(child);
                info!("usb: port {place}: device removed");
            }
        }
        for place in arrived {
            match self.enumerate_at(place) {
                Ok(Some(description)) => info!("usb: port {place}: {description}"),
                Ok(None) => {}
                Err(error) => warn!("usb: port {place}: cannot use the device: {error}"),
            }
        }
    }

    fn setup_device(&mut self, slot: u8, place: Place) -> Result<Option<String>, XhciError> {
        let speed = place.speed;
        let output = DmaPage::new()?;
        let input = DmaPage::new()?;
        let scratch = DmaPage::new()?;
        let mut ring = Ring::new()?;
        self.device_contexts
            .write64(usize::from(slot) * 8, output.phys);

        // The default control endpoint's packet size is fixed by the
        // speed, except at full speed, where the device says (8 is the
        // minimum and safe to start with).
        let mut packet_size: u32 = match speed {
            SPEED_LOW | SPEED_FULL => 8,
            SPEED_HIGH => 64,
            _ => 512,
        };
        let slot_context = self.input_context(0);
        let endpoint0 = self.input_context(1);
        // Input control context: add the slot context and endpoint 0.
        input.write32(4, 0b11);
        // One context entry (endpoint 0), and where the device is.
        for (index, word) in place.slot_words(1, None).into_iter().enumerate() {
            input.write32(slot_context + 4 * index, word);
        }
        // Error count 3, control endpoint, packet size.
        input.write32(
            endpoint0 + 4,
            (3 << 1) | (ENDPOINT_CONTROL << 3) | (packet_size << 16),
        );
        input.write64(endpoint0 + 8, ring.page.phys | u64::from(TRB_CYCLE));
        input.write32(endpoint0 + 16, 8);
        let slot_bits = u32::from(slot) << 24;
        self.command(input.phys, (TRB_ADDRESS_DEVICE << 10) | slot_bits)?;

        // Device descriptor: first the part every device can send in one
        // packet, which says how large packets may be.
        let mut device = [0u8; 18];
        self.control(
            slot,
            &mut ring,
            scratch,
            [0x80, 6, 0, 1, 0, 0, 8, 0],
            Data::In(&mut device[..8]),
        )?;
        if speed == SPEED_FULL && u32::from(device[7]) != packet_size {
            packet_size = match device[7] {
                size @ (8 | 16 | 32 | 64) => u32::from(size),
                _ => return Err(XhciError::Protocol),
            };
            input.write32(0, 0);
            input.write32(4, 0b10);
            input.write32(
                endpoint0 + 4,
                (3 << 1) | (ENDPOINT_CONTROL << 3) | (packet_size << 16),
            );
            self.command(input.phys, (TRB_EVALUATE_CONTEXT << 10) | slot_bits)?;
        }
        self.control(
            slot,
            &mut ring,
            scratch,
            [0x80, 6, 0, 1, 0, 0, 18, 0],
            Data::In(&mut device),
        )?;
        let (vendor, product) = (
            u16::from_le_bytes([device[8], device[9]]),
            u16::from_le_bytes([device[10], device[11]]),
        );

        // The first configuration, with its interfaces and endpoints.
        let mut configuration = [0u8; 255];
        self.control(
            slot,
            &mut ring,
            scratch,
            [0x80, 6, 0, 2, 0, 0, 255, 0],
            Data::In(&mut configuration),
        )?;
        let total = usize::from(u16::from_le_bytes([configuration[2], configuration[3]]))
            .min(configuration.len());
        if device[4] == 9 {
            // Device class 9: a hub.
            return self.setup_hub(slot, place, ring, scratch, input, configuration[5]);
        }
        let found = boot_interfaces(&configuration[..total]);
        if found.is_empty() {
            // Not a keyboard or mouse: leave it unconfigured.
            info!(
                "usb: port {place}: device {vendor:04x}:{product:04x} is not a boot keyboard \
                 or mouse; ignored"
            );
            return Ok(None);
        }
        // Select the configuration.
        self.control(
            slot,
            &mut ring,
            scratch,
            [0x00, 9, configuration[5], 0, 0, 0, 0, 0],
            Data::None,
        )?;

        let mut names = Vec::new();
        let mut add_flags = 1u32;
        let mut last_index = 1u8;
        input.clear();
        for interface in &found {
            // Boot protocol (0), and "only report changes" (idle rate
            // 0); some devices reject the latter, which does not matter.
            self.control(
                slot,
                &mut ring,
                scratch,
                [0x21, 0x0B, 0, 0, interface.number, 0, 0, 0],
                Data::None,
            )?;
            let _ = self.control(
                slot,
                &mut ring,
                scratch,
                [0x21, 0x0A, 0, 0, interface.number, 0, 0, 0],
                Data::None,
            );
            let endpoint_ring = Ring::new()?;
            let index = (interface.endpoint & 0x0F) * 2 + 1;
            let context = self.input_context(usize::from(index));
            let packet = u32::from(interface.packet_size);
            // Polling period as a power of two of 125 µs: the device
            // gives milliseconds (low and full speed) or that exponent
            // plus one (high speed and above).
            let interval = match speed {
                SPEED_LOW | SPEED_FULL => (u32::from(interface.interval.max(1)) * 8)
                    .ilog2()
                    .clamp(3, 10),
                _ => u32::from(interface.interval.clamp(1, 16)) - 1,
            };
            input.write32(context, interval << 16);
            input.write32(
                context + 4,
                (3 << 1) | (ENDPOINT_INTERRUPT_IN << 3) | (packet << 16),
            );
            input.write64(context + 8, endpoint_ring.page.phys | u64::from(TRB_CYCLE));
            // Average transfer size, and the most data per interval.
            input.write32(context + 16, packet | (packet << 16));
            add_flags |= 1 << index;
            last_index = last_index.max(index);
            self.endpoints.push(Endpoint {
                slot,
                index,
                kind: interface.kind,
                ring: endpoint_ring,
                buffers: DmaPage::new()?,
                next_buffer: 0,
                report_size: usize::from(interface.packet_size).min(REPORT_BUFFER_SIZE),
                keyboard: BootKeyboard::new(),
                decoder: Decoder::new(),
                repeat_at: None,
            });
            names.push(match interface.kind {
                Kind::Keyboard => "keyboard",
                Kind::Mouse => "mouse",
            });
        }
        input.write32(4, add_flags);
        let words = place.slot_words(u32::from(last_index), None);
        for (index, word) in words.into_iter().enumerate() {
            input.write32(slot_context + 4 * index, word);
        }
        self.command(input.phys, (TRB_CONFIGURE_ENDPOINT << 10) | slot_bits)?;

        // Start listening: queue transfers for the reports.
        for position in 0..self.endpoints.len() {
            if self.endpoints[position].slot == slot {
                for _ in 0..REPORT_BUFFERS {
                    self.queue_report(position);
                }
            }
        }
        self.devices.push(Device {
            slot,
            place,
            hub: None,
        });
        Ok(Some(alloc::format!(
            "{} ({vendor:04x}:{product:04x})",
            names.join(" and ")
        )))
    }

    /// Queues one more transfer on an interrupt endpoint.
    fn queue_report(&mut self, position: usize) {
        let endpoint = &mut self.endpoints[position];
        let buffer = endpoint.buffers.phys + (endpoint.next_buffer * REPORT_BUFFER_SIZE) as u64;
        endpoint.next_buffer = (endpoint.next_buffer + 1) % REPORT_BUFFERS;
        endpoint.ring.push(
            buffer,
            endpoint.report_size as u32,
            (TRB_NORMAL << 10) | TRB_INTERRUPT | TRB_INTERRUPT_ON_SHORT,
        );
        let (slot, index) = (endpoint.slot, endpoint.index);
        self.ring_doorbell(slot, u32::from(index));
    }

    /// Handles an event nobody was waiting for. Port changes are only
    /// noted: acting on one means running commands, which must not start
    /// while another command or transfer is being waited for.
    fn handle(&mut self, event: Event) {
        match event.kind() {
            EVENT_TRANSFER => self.report_arrived(event),
            EVENT_PORT => self.changed_ports.push((event.parameter >> 24) as u8),
            _ => {}
        }
    }

    fn report_arrived(&mut self, event: Event) {
        let Some(position) = self.endpoints.iter().position(|endpoint| {
            endpoint.slot == event.slot() && endpoint.index == event.endpoint()
        }) else {
            return;
        };
        if !matches!(
            event.completion(),
            COMPLETION_SUCCESS | COMPLETION_SHORT_PACKET
        ) {
            // The endpoint has stopped (the device stalled or is gone).
            // It is left alone; unplugging and replugging starts afresh.
            return;
        }
        let endpoint = &mut self.endpoints[position];
        // The event names the transfer's TRB; the TRB names the buffer.
        let Some(offset) = event
            .parameter
            .checked_sub(endpoint.ring.page.phys)
            .filter(|offset| *offset < PAGE_SIZE - 16)
        else {
            return;
        };
        let buffer = u64::from(endpoint.ring.page.read32(offset as usize))
            | (u64::from(endpoint.ring.page.read32(offset as usize + 4)) << 32);
        let Some(buffer_offset) = buffer
            .checked_sub(endpoint.buffers.phys)
            .filter(|offset| *offset as usize + REPORT_BUFFER_SIZE <= PAGE_SIZE as usize)
        else {
            return;
        };
        // The event's length field is what was *not* transferred.
        let received = endpoint
            .report_size
            .saturating_sub((event.status & 0xFF_FFFF) as usize);
        let mut report = [0u8; REPORT_BUFFER_SIZE];
        endpoint
            .buffers
            .bytes(buffer_offset as usize, &mut report[..received]);
        match endpoint.kind {
            Kind::Keyboard => {
                let Endpoint {
                    keyboard,
                    decoder,
                    repeat_at,
                    ..
                } = endpoint;
                let pressed = keyboard.feed(&report[..received], |scancode| {
                    if let Some(key) = decoder.feed_key(scancode) {
                        input::key_pressed(key);
                    }
                });
                if pressed {
                    *repeat_at = Some(time::ticks() + REPEAT_DELAY_TICKS);
                }
            }
            Kind::Mouse => {
                if let Some(movement) = hid::boot_mouse(&report[..received]) {
                    input::mouse_moved(movement);
                }
            }
        }
        self.queue_report(position);
    }

    /// Repeats keys that are being held down.
    fn repeat_keys(&mut self) {
        let now = time::ticks();
        for endpoint in &mut self.endpoints {
            let Endpoint {
                keyboard,
                decoder,
                repeat_at,
                ..
            } = endpoint;
            if repeat_at.is_some_and(|at| now >= at) {
                let repeated = keyboard.repeat(|scancode| {
                    if let Some(key) = decoder.feed_key(scancode) {
                        input::key_pressed(key);
                    }
                });
                *repeat_at = repeated.then_some(now + REPEAT_INTERVAL_TICKS);
            }
        }
    }

    /// A device was plugged in or removed.
    fn port_changed(&mut self, port: u8) {
        if port == 0 || port > self.ports {
            return;
        }
        let register = self.port_register(port);
        let status = self.read32(register);
        self.write32(register, (status & PORT_PRESERVE) | PORT_CHANGES);
        let known = self.root_device(port);
        match (status & PORT_CONNECTED != 0, known) {
            (true, None) => self.attach(port),
            (false, Some(slot)) => {
                self.detach(slot);
                info!("usb: port {port}: device removed");
            }
            _ => {}
        }
    }

    /// The slot of the device plugged directly into the controller's
    /// port `port`, if one has been set up.
    fn root_device(&self, port: u8) -> Option<u8> {
        self.devices
            .iter()
            .find(|device| device.place.parent.is_none() && device.place.root_port == port)
            .map(|device| device.slot)
    }

    /// Looks at the ports of every hub.
    fn scan_hubs(&mut self) {
        let hubs: Vec<u8> = self
            .devices
            .iter()
            .filter(|device| device.hub.is_some())
            .map(|device| device.slot)
            .collect();
        for slot in hubs {
            self.scan_hub(slot);
        }
    }

    fn attach(&mut self, port: u8) {
        match self.enumerate(port) {
            Ok(Some(description)) => info!("usb: port {port}: {description}"),
            Ok(None) => {}
            Err(error) => warn!("usb: port {port}: cannot use the device: {error}"),
        }
    }
}

/// Register space mapped. The registers of real controllers fit in 64
/// KiB; anything pointing beyond is refused.
const MAPPED_BYTES: u64 = 0x1_0000;

/// A boot keyboard or mouse interface of a device.
struct Interface {
    number: u8,
    kind: Kind,
    /// Address of its interrupt IN endpoint.
    endpoint: u8,
    packet_size: u16,
    interval: u8,
}

/// Finds the HID boot interfaces (at most one keyboard and one mouse) in
/// a configuration descriptor, with their interrupt IN endpoints.
fn boot_interfaces(configuration: &[u8]) -> Vec<Interface> {
    const INTERFACE: u8 = 4;
    const ENDPOINT: u8 = 5;
    let mut found: Vec<Interface> = Vec::new();
    // The interface being looked at, until its endpoint turns up.
    let mut current: Option<(u8, Kind)> = None;
    let mut rest = configuration;
    while let [length, kind, ..] = *rest {
        let length = usize::from(length);
        if length < 2 || length > rest.len() {
            break;
        }
        let descriptor = &rest[..length];
        rest = &rest[length..];
        match kind {
            INTERFACE if length >= 9 => {
                // Class 3 (HID), subclass 1 (boot), protocol 1 or 2.
                current = match (descriptor[5], descriptor[6], descriptor[7]) {
                    (3, 1, 1) => Some((descriptor[2], Kind::Keyboard)),
                    (3, 1, 2) => Some((descriptor[2], Kind::Mouse)),
                    _ => None,
                }
                .filter(|(_, kind)| found.iter().all(|other| other.kind != *kind));
            }
            ENDPOINT if length >= 7 => {
                // An interrupt endpoint (type 3) towards the host.
                let (address, attributes) = (descriptor[2], descriptor[3]);
                if address & 0x80 != 0
                    && attributes & 3 == 3
                    && address & 0x0F != 0
                    && let Some((number, kind)) = current.take()
                {
                    found.push(Interface {
                        number,
                        kind,
                        endpoint: address,
                        packet_size: u16::from_le_bytes([descriptor[4], descriptor[5]]) & 0x7FF,
                        interval: descriptor[6],
                    });
                }
            }
            _ => {}
        }
    }
    found
}

/// The driver's thread: enumerates what is plugged in, then keeps
/// reading events.
fn run(controller: usize) {
    // SAFETY: `init` leaked the controller into this pointer and hands
    // it to this thread only.
    let xhci = unsafe { &mut *(controller as *mut Xhci) };
    for port in 1..=xhci.ports {
        // Power the port if it is not (most are from reset).
        let register = xhci.port_register(port);
        let status = xhci.read32(register);
        if status & PORT_POWER == 0 {
            xhci.write32(register, (status & PORT_PRESERVE) | PORT_POWER);
        }
    }
    // Devices need a moment after power-up before they answer.
    sched::sleep_ticks(u64::from(time::TICK_HZ) / 5);
    for port in 1..=xhci.ports {
        if xhci.root_device(port).is_none() {
            xhci.attach(port);
        }
    }
    INITIAL_SCAN_DONE.store(true, Ordering::Release);
    let mut next_hub_scan = time::ticks();
    loop {
        while let Some(event) = xhci.next_event() {
            xhci.handle(event);
        }
        while let Some(port) = xhci.changed_ports.pop() {
            xhci.port_changed(port);
        }
        // Hubs are asked about their ports twice a second (their own
        // way of announcing changes, an interrupt endpoint, is not
        // used).
        if time::ticks() >= next_hub_scan {
            next_hub_scan = time::ticks() + HUB_SCAN_TICKS;
            xhci.scan_hubs();
        }
        xhci.repeat_keys();
        sched::sleep_ticks(1);
    }
}

/// Finds the first xHCI controller, starts it and the thread that
/// serves it. Returns the number of ports, or `None` without a
/// controller.
pub fn init() -> Result<Option<u8>, XhciError> {
    let Some(pci) = pci::devices()
        .into_iter()
        .find(|device| (device.class, device.subclass, device.prog_if) == CLASS)
    else {
        return Ok(None);
    };
    let xhci = alloc::boxed::Box::new(Xhci::new(&pci)?);
    let ports = xhci.ports;
    registry::bind(
        "xhci",
        Origin::Native,
        &alloc::format!("{:02x}:{:02x}.{}", pci.bus, pci.device, pci.function),
    );
    let controller = alloc::boxed::Box::into_raw(xhci) as usize;
    sched::spawn("usb", run, controller).map_err(|_| XhciError::OutOfMemory)?;
    // Wait for the devices already plugged in, so that a keyboard works
    // as soon as there is something to type into — but not for ever: a
    // device that does not answer costs its time-outs, not the boot.
    let deadline = time::ticks() + INITIAL_SCAN_TICKS;
    while !INITIAL_SCAN_DONE.load(Ordering::Acquire) && time::ticks() < deadline {
        sched::sleep_ticks(1);
    }
    Ok(Some(ports))
}
