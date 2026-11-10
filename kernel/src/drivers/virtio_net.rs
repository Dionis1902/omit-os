// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! VirtIO network card driver (VirtIO 1.x over PCI).
//!
//! As simple as the block driver next to it:
//!
//! * sixteen receive buffers of one page each, handed back to the device
//!   as soon as their frame has been copied out;
//! * one frame sent at a time through one page, waiting for the device to
//!   take it;
//! * no interrupts: [`crate::net`] asks for received frames;
//! * no offloading of any kind, so every frame is complete and carries
//!   its own checksums, and the only optional feature used is the one
//!   that tells the driver the card's hardware address.
//!
//! The link state is not looked at: a card without a cable simply
//! receives nothing.

use alloc::boxed::Box;
use alloc::vec::Vec;

use omit_net::MacAddr;
use omit_net::ethernet::MAX_FRAME_LEN;

use super::registry::{self, Origin};
use super::virtio::{self, DESC_WRITE, DmaPage, Transport, VirtioError, Virtqueue};
use crate::mm::PAGE_SIZE;
use crate::net::{self, NetDevice, NetError};
use crate::pci::{self, PciDevice};
use crate::{sched, time};

/// Modern (0x1041) and transitional (0x1000) network device IDs.
const DEVICE_IDS: [u16; 2] = [0x1041, 0x1000];

/// The device has a hardware address in its configuration structure.
const FEATURE_MAC: u32 = 1 << 5;

const QUEUE_RECEIVE: u16 = 0;
const QUEUE_TRANSMIT: u16 = 1;
/// Entries per queue, and the number of receive buffers.
const QUEUE_SIZE: u16 = 16;

/// Every frame is preceded by a header about offloading, which is not
/// used: twelve zero bytes when sending, twelve bytes skipped when
/// receiving.
const HEADER_LEN: usize = 12;

/// How long to wait for the device to take a frame.
const TIMEOUT_TICKS: u64 = time::TICK_HZ as u64;

/// An initialised VirtIO network card.
pub struct VirtioNet {
    mac: MacAddr,
    receive: Virtqueue,
    transmit: Virtqueue,
    /// Receive buffer `i` belongs to descriptor `i` of the receive queue.
    receive_pages: Vec<DmaPage>,
    transmit_page: DmaPage,
}

impl VirtioNet {
    /// Brings the device at `pci` to the "driver OK" state with all its
    /// receive buffers available.
    fn new(pci: &PciDevice) -> Result<Self, VirtioError> {
        let transport = Transport::begin(pci, FEATURE_MAC)?;
        let mac = if transport.features & FEATURE_MAC != 0 {
            let mut mac = [0u8; 6];
            for (offset, byte) in mac.iter_mut().enumerate() {
                *byte = transport.config.read8(offset as u64);
            }
            MacAddr(mac)
        } else {
            // The device leaves the choice to the driver: a locally
            // administered address derived from where the card sits.
            MacAddr([0x02, 0x4F, 0x4D, pci.bus, pci.device, pci.function])
        };
        let mut device = Self {
            mac,
            receive: transport.queue(QUEUE_RECEIVE, QUEUE_SIZE)?,
            transmit: transport.queue(QUEUE_TRANSMIT, QUEUE_SIZE)?,
            receive_pages: Vec::new(),
            transmit_page: DmaPage::new()?,
        };
        for index in 0..QUEUE_SIZE {
            let page = DmaPage::new()?;
            device
                .receive
                .set_descriptor(index, page.phys, PAGE_SIZE as u32, DESC_WRITE, 0);
            device.receive_pages.push(page);
        }
        transport.start();
        for index in 0..QUEUE_SIZE {
            device.receive.submit(index);
        }
        Ok(device)
    }
}

impl NetDevice for VirtioNet {
    fn mac(&self) -> MacAddr {
        self.mac
    }

    fn transmit(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if frame.len() > MAX_FRAME_LEN {
            return Err(NetError::TooLong);
        }
        // SAFETY: the transmit page is driver-owned and the device is not
        // reading it (one frame at a time, and the previous one has been
        // handed back); header and frame fit in it.
        unsafe {
            self.transmit_page.virt.write_bytes(0, HEADER_LEN);
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                self.transmit_page.virt.add(HEADER_LEN),
                frame.len(),
            );
        }
        self.transmit.set_descriptor(
            0,
            self.transmit_page.phys,
            (HEADER_LEN + frame.len()) as u32,
            0,
            0,
        );
        self.transmit.submit(0);
        let deadline = time::ticks() + TIMEOUT_TICKS;
        while self.transmit.pop_used().is_none() {
            if time::ticks() > deadline {
                return Err(NetError::Timeout);
            }
            sched::yield_now();
        }
        Ok(())
    }

    fn receive(&mut self, buffer: &mut [u8]) -> Option<usize> {
        loop {
            let (head, written) = self.receive.pop_used()?;
            // A device that names a buffer it was never given is ignored.
            let Some(page) = self.receive_pages.get(usize::from(head)) else {
                continue;
            };
            let length = (written as usize)
                .min(PAGE_SIZE as usize)
                .saturating_sub(HEADER_LEN)
                .min(buffer.len());
            // SAFETY: the page is driver-owned, the device has finished
            // writing it (it handed the buffer back), and header plus
            // `length` bytes are within the page.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    page.virt.add(HEADER_LEN),
                    buffer.as_mut_ptr(),
                    length,
                );
            }
            // The descriptor still describes the page: lend it again.
            self.receive.submit(head);
            return Some(length);
        }
    }
}

/// Finds the first VirtIO network card, initialises it and registers it
/// as the machine's network interface. Returns its hardware address, or
/// `None` if there is no such device.
pub fn init() -> Result<Option<MacAddr>, VirtioError> {
    let Some(pci) = pci::find(virtio::VENDOR, &DEVICE_IDS) else {
        return Ok(None);
    };
    let device = VirtioNet::new(&pci)?;
    let mac = device.mac;
    net::register("virtio-net", Box::new(device));
    registry::bind(
        "virtio-net",
        Origin::Native,
        &alloc::format!("{:02x}:{:02x}.{}", pci.bus, pci.device, pci.function),
    );
    Ok(Some(mac))
}
