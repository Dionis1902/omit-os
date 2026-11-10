// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Driver for Intel's 8254x gigabit Ethernet cards ("e1000"): the card
//! QEMU calls `e1000` and VirtualBox offers by default (82540EM), and its
//! close relatives.
//!
//! Written from Intel's "PCI/PCI-X Family of Gigabit Ethernet Controllers
//! Software Developer's Manual" (the register and descriptor layouts are
//! chapters 3 and 13 of it). As simple as the VirtIO driver next to it:
//!
//! * thirty-two receive buffers of one page each, a frame to a buffer;
//! * one frame sent at a time, waiting for the card to say it is done;
//! * no interrupts: [`crate::net`] asks for received frames;
//! * no offloading, no VLANs, no flow control, no jumbo frames; the card
//!   negotiates speed and duplex by itself.
//!
//! The link state is not looked at: a card without a cable simply
//! receives nothing.

use alloc::boxed::Box;
use alloc::vec::Vec;

use omit_net::MacAddr;
use omit_net::ethernet::MAX_FRAME_LEN;

use super::registry::{self, Origin};
use crate::arch::paging::{self, MapError};
use crate::mm::{self, PAGE_SIZE, frame};
use crate::net::{self, NetDevice, NetError};
use crate::pci::{self, COMMAND_BUS_MASTER, COMMAND_MEMORY, PciDevice};
use crate::{sched, time};

const VENDOR_INTEL: u16 = 0x8086;
/// 82540EM (QEMU's `e1000`, VirtualBox's default) and 82545EM copper
/// (QEMU's `e1000-82545em`, VMware's): programmed the same way as far as
/// this driver goes. Other members of the family differ in details this
/// driver touches (how the EEPROM is read, for one) and are left alone
/// until one of them can be tried.
const DEVICE_IDS: [u16; 2] = [0x100E, 0x100F];

/// Bytes of registers mapped: everything up to the address filters.
const REGISTER_BYTES: u64 = 0x2_0000;

// Registers (offsets into the first memory region of the card).
const REG_CTRL: u64 = 0x0000;
const REG_EERD: u64 = 0x0014;
const REG_IMC: u64 = 0x00D8;
const REG_RCTL: u64 = 0x0100;
const REG_TCTL: u64 = 0x0400;
const REG_TIPG: u64 = 0x0410;
const REG_RDBAL: u64 = 0x2800;
const REG_RDBAH: u64 = 0x2804;
const REG_RDLEN: u64 = 0x2808;
const REG_RDH: u64 = 0x2810;
const REG_RDT: u64 = 0x2818;
const REG_TDBAL: u64 = 0x3800;
const REG_TDBAH: u64 = 0x3804;
const REG_TDLEN: u64 = 0x3808;
const REG_TDH: u64 = 0x3810;
const REG_TDT: u64 = 0x3818;
/// The multicast table: 128 words.
const REG_MTA: u64 = 0x5200;
/// The first receive address: low and high halves.
const REG_RAL0: u64 = 0x5400;
const REG_RAH0: u64 = 0x5404;

// Device control.
const CTRL_LINK_RESET: u32 = 1 << 3;
const CTRL_AUTO_SPEED: u32 = 1 << 5;
const CTRL_SET_LINK_UP: u32 = 1 << 6;
const CTRL_INVERT_LOSS_OF_SIGNAL: u32 = 1 << 7;
const CTRL_RESET: u32 = 1 << 26;
const CTRL_PHY_RESET: u32 = 1 << 31;

// EEPROM read.
const EERD_START: u32 = 1 << 0;
const EERD_DONE: u32 = 1 << 4;

// Receive control: enabled, broadcasts accepted, 2048-byte buffers (the
// buffer-size field left at zero), the frame check sequence stripped.
const RCTL_ENABLE: u32 = 1 << 1;
const RCTL_BROADCAST: u32 = 1 << 15;
const RCTL_STRIP_CRC: u32 = 1 << 26;

// Transmit control: enabled, short frames padded, the collision
// threshold and distance the manual gives for full duplex.
const TCTL_ENABLE: u32 = 1 << 1;
const TCTL_PAD_SHORT: u32 = 1 << 3;
const TCTL_COLLISION_THRESHOLD: u32 = 0x0F << 4;
const TCTL_COLLISION_DISTANCE: u32 = 0x40 << 12;
/// Inter-packet gaps for copper, as the manual gives them: 10, 8, 6.
const TIPG_COPPER: u32 = 10 | (8 << 10) | (6 << 20);

const RAH_VALID: u32 = 1 << 31;

/// A descriptor of either ring is sixteen bytes: the buffer's address,
/// then fields that differ.
const DESCRIPTOR: usize = 16;
// Receive descriptor: length at 8 (16 bits), status at 12.
// Transmit descriptor: length at 8 (16 bits), command at 11, status at 12.
const OFFSET_LENGTH: usize = 8;
const OFFSET_COMMAND: usize = 11;
const OFFSET_STATUS: usize = 12;
/// Status: the card is done with the descriptor.
const STATUS_DONE: u8 = 1 << 0;
/// Receive status: the buffer holds the end of a frame.
const STATUS_END_OF_PACKET: u8 = 1 << 1;
/// Transmit command: end of the frame, add the check sequence, report
/// when done.
const COMMAND_SEND: u8 = (1 << 0) | (1 << 1) | (1 << 3);

/// Receive buffers. (The ring's length in bytes must be a multiple of
/// 128: eight descriptors.)
const RECEIVE_COUNT: usize = 32;
/// Transmit descriptors; one is in use at a time.
const TRANSMIT_COUNT: usize = 8;

/// How long to wait for the card to reset, read its EEPROM, or send a
/// frame.
const TIMEOUT_TICKS: u64 = time::TICK_HZ as u64;

/// Why the card could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum E1000Error {
    /// The card has no memory-mapped registers assigned.
    NoRegisters,
    /// Its registers could not be mapped.
    Map(MapError),
    /// No memory for rings and buffers.
    OutOfMemory,
    /// The card did not come out of reset.
    ResetTimeout,
    /// The card has no usable hardware address.
    NoAddress,
}

impl core::fmt::Display for E1000Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoRegisters => f.write_str("the card has no register memory assigned"),
            Self::Map(error) => write!(f, "cannot map the card's registers: {error:?}"),
            Self::OutOfMemory => f.write_str("out of memory"),
            Self::ResetTimeout => f.write_str("the card did not come out of reset"),
            Self::NoAddress => f.write_str("the card has no hardware address"),
        }
    }
}

/// One page of memory the card may read and write, owned by the driver.
struct Page {
    phys: u64,
    virt: *mut u8,
}

// SAFETY: the page is exclusively owned by the driver that holds the
// `Page`; the raw pointer is just its direct-map address.
unsafe impl Send for Page {}

impl Page {
    fn new() -> Result<Self, E1000Error> {
        let phys = frame::alloc_zeroed(0).ok_or(E1000Error::OutOfMemory)?;
        let virt = mm::phys_to_virt(phys, PAGE_SIZE).ok_or(E1000Error::OutOfMemory)?;
        Ok(Self { phys, virt })
    }

    /// Reads the byte at `offset`, which the card may have written.
    fn read8(&self, offset: usize) -> u8 {
        debug_assert!(offset < PAGE_SIZE as usize);
        // SAFETY: the page is owned by the driver and `offset` is within
        // it (all callers pass descriptor offsets below 4096). The card
        // may write the page at any time, hence the volatile read.
        unsafe { self.virt.add(offset).read_volatile() }
    }

    fn read16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.read8(offset), self.read8(offset + 1)])
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) {
        debug_assert!(offset + bytes.len() <= PAGE_SIZE as usize);
        for (index, &byte) in bytes.iter().enumerate() {
            // SAFETY: as in `read8`; the range was checked by the caller
            // (descriptor fields and frames of at most 1514 bytes).
            unsafe { self.virt.add(offset + index).write_volatile(byte) };
        }
    }
}

/// An initialised card.
pub struct E1000 {
    /// Where the card's registers are mapped.
    registers: u64,
    mac: MacAddr,
    receive_ring: Page,
    receive_buffers: Vec<Page>,
    /// The receive descriptor the card fills next.
    receive_next: usize,
    transmit_ring: Page,
    transmit_buffer: Page,
    /// The transmit descriptor used next.
    transmit_next: usize,
}

impl E1000 {
    fn read32(&self, register: u64) -> u32 {
        // SAFETY: `registers` is a device mapping of `REGISTER_BYTES`
        // bytes made in `new`; every register offset used is below that
        // and a multiple of four. Device registers are read volatile.
        unsafe { ((self.registers + register) as *const u32).read_volatile() }
    }

    fn write32(&self, register: u64, value: u32) {
        // SAFETY: as in `read32`.
        unsafe { ((self.registers + register) as *mut u32).write_volatile(value) }
    }

    /// Waits until `done` says so, for at most a second.
    fn wait(&self, mut done: impl FnMut(&Self) -> bool) -> bool {
        let deadline = time::ticks() + TIMEOUT_TICKS;
        while !done(self) {
            if time::ticks() > deadline {
                return false;
            }
            sched::yield_now();
        }
        true
    }

    /// One 16-bit word of the card's EEPROM.
    fn eeprom(&self, word: u32) -> Option<u16> {
        self.write32(REG_EERD, EERD_START | (word << 8));
        self.wait(|card| card.read32(REG_EERD) & EERD_DONE != 0)
            .then(|| (self.read32(REG_EERD) >> 16) as u16)
    }

    /// The card's hardware address: what the reset loaded into the first
    /// receive address register, or failing that the EEPROM's words.
    fn read_mac(&self) -> Option<MacAddr> {
        let usable = |mac: [u8; 6]| mac != [0; 6] && mac != [0xFF; 6] && mac[0] & 1 == 0;
        let (low, high) = (self.read32(REG_RAL0), self.read32(REG_RAH0));
        let loaded = [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ];
        if usable(loaded) {
            return Some(MacAddr(loaded));
        }
        let mut stored = [0u8; 6];
        for word in 0..3 {
            let value = self.eeprom(word)?;
            stored[word as usize * 2..][..2].copy_from_slice(&value.to_le_bytes());
        }
        usable(stored).then_some(MacAddr(stored))
    }

    /// Resets the card at `pci` and starts its receiver and transmitter.
    fn new(pci: &PciDevice) -> Result<Self, E1000Error> {
        let bar = pci.memory_bar(0).ok_or(E1000Error::NoRegisters)?;
        // SAFETY: this driver is taking over the device; nothing else
        // uses it. Memory decoding is needed for the registers, bus
        // mastering for the card to read and write rings and buffers.
        unsafe { pci.enable(COMMAND_MEMORY | COMMAND_BUS_MASTER) };
        let registers = paging::map_mmio(bar.address, REGISTER_BYTES).map_err(E1000Error::Map)?;
        let mut card = Self {
            registers,
            mac: MacAddr([0; 6]),
            receive_ring: Page::new()?,
            receive_buffers: Vec::new(),
            receive_next: 0,
            transmit_ring: Page::new()?,
            transmit_buffer: Page::new()?,
            transmit_next: 0,
        };

        // No interrupts, before and after the reset (which turns them
        // back to their defaults).
        card.write32(REG_IMC, u32::MAX);
        card.write32(REG_CTRL, card.read32(REG_CTRL) | CTRL_RESET);
        if !card.wait(|card| card.read32(REG_CTRL) & CTRL_RESET == 0) {
            return Err(E1000Error::ResetTimeout);
        }
        card.write32(REG_IMC, u32::MAX);
        let control = card.read32(REG_CTRL);
        card.write32(
            REG_CTRL,
            (control | CTRL_SET_LINK_UP | CTRL_AUTO_SPEED)
                & !(CTRL_LINK_RESET | CTRL_PHY_RESET | CTRL_INVERT_LOSS_OF_SIGNAL),
        );

        card.mac = card.read_mac().ok_or(E1000Error::NoAddress)?;
        // Frames for this address only (and broadcasts): the address in
        // the first filter, the multicast table empty.
        let mac = card.mac.0;
        card.write32(
            REG_RAL0,
            u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]),
        );
        card.write32(
            REG_RAH0,
            u32::from(u16::from_le_bytes([mac[4], mac[5]])) | RAH_VALID,
        );
        for index in 0..128 {
            card.write32(REG_MTA + index * 4, 0);
        }

        // The receive ring: every descriptor points at a page of its
        // own. The card owns the descriptors from head up to, but not
        // including, tail: all but one.
        for index in 0..RECEIVE_COUNT {
            let buffer = Page::new()?;
            card.receive_ring
                .write(index * DESCRIPTOR, &buffer.phys.to_le_bytes());
            card.receive_buffers.push(buffer);
        }
        card.write32(REG_RDBAL, card.receive_ring.phys as u32);
        card.write32(REG_RDBAH, (card.receive_ring.phys >> 32) as u32);
        card.write32(REG_RDLEN, (RECEIVE_COUNT * DESCRIPTOR) as u32);
        card.write32(REG_RDH, 0);
        card.write32(REG_RDT, RECEIVE_COUNT as u32 - 1);
        card.write32(REG_RCTL, RCTL_ENABLE | RCTL_BROADCAST | RCTL_STRIP_CRC);

        // The transmit ring: every descriptor points at the one buffer.
        for index in 0..TRANSMIT_COUNT {
            let address = card.transmit_buffer.phys.to_le_bytes();
            card.transmit_ring.write(index * DESCRIPTOR, &address);
        }
        card.write32(REG_TDBAL, card.transmit_ring.phys as u32);
        card.write32(REG_TDBAH, (card.transmit_ring.phys >> 32) as u32);
        card.write32(REG_TDLEN, (TRANSMIT_COUNT * DESCRIPTOR) as u32);
        card.write32(REG_TDH, 0);
        card.write32(REG_TDT, 0);
        card.write32(REG_TIPG, TIPG_COPPER);
        card.write32(
            REG_TCTL,
            TCTL_ENABLE | TCTL_PAD_SHORT | TCTL_COLLISION_THRESHOLD | TCTL_COLLISION_DISTANCE,
        );
        Ok(card)
    }
}

impl NetDevice for E1000 {
    fn mac(&self) -> MacAddr {
        self.mac
    }

    fn transmit(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if frame.len() > MAX_FRAME_LEN {
            return Err(NetError::TooLong);
        }
        // The card is not reading the buffer: the frame before this one
        // was waited for.
        self.transmit_buffer.write(0, frame);
        let descriptor = self.transmit_next * DESCRIPTOR;
        self.transmit_ring.write(
            descriptor + OFFSET_LENGTH,
            &(frame.len() as u16).to_le_bytes(),
        );
        self.transmit_ring
            .write(descriptor + OFFSET_COMMAND, &[COMMAND_SEND, 0]);
        self.transmit_next = (self.transmit_next + 1) % TRANSMIT_COUNT;
        self.write32(REG_TDT, self.transmit_next as u32);
        let sent = self
            .wait(|card| card.transmit_ring.read8(descriptor + OFFSET_STATUS) & STATUS_DONE != 0);
        if sent { Ok(()) } else { Err(NetError::Timeout) }
    }

    fn receive(&mut self, buffer: &mut [u8]) -> Option<usize> {
        loop {
            let index = self.receive_next;
            let descriptor = index * DESCRIPTOR;
            let status = self.receive_ring.read8(descriptor + OFFSET_STATUS);
            if status & STATUS_DONE == 0 {
                return None;
            }
            let length = usize::from(self.receive_ring.read16(descriptor + OFFSET_LENGTH))
                .min(PAGE_SIZE as usize);
            let whole = status & STATUS_END_OF_PACKET != 0;
            let count = length.min(buffer.len());
            if whole {
                let page = &self.receive_buffers[index];
                for (offset, byte) in buffer[..count].iter_mut().enumerate() {
                    *byte = page.read8(offset);
                }
            }
            // Hand the descriptor back: status cleared, and the tail
            // moved onto it (the card then owns everything before it).
            self.receive_ring
                .write(descriptor + OFFSET_STATUS, &[0, 0, 0, 0]);
            self.receive_next = (index + 1) % RECEIVE_COUNT;
            self.write32(REG_RDT, index as u32);
            // A frame spread over several buffers is larger than any
            // this driver asked for: its parts are dropped.
            if whole {
                return Some(count);
            }
        }
    }
}

/// Finds the first card of the family, starts it and registers it as the
/// machine's network interface. Returns its hardware address, or `None`
/// if there is no such card.
pub fn init() -> Result<Option<MacAddr>, E1000Error> {
    let Some(pci) = pci::find(VENDOR_INTEL, &DEVICE_IDS) else {
        return Ok(None);
    };
    let card = E1000::new(&pci)?;
    let mac = card.mac;
    net::register("e1000", Box::new(card));
    registry::bind(
        "e1000",
        Origin::Native,
        &alloc::format!("{:02x}:{:02x}.{}", pci.bus, pci.device, pci.function),
    );
    Ok(Some(mac))
}
