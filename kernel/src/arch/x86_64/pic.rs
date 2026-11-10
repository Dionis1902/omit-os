// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Legacy 8259 programmable interrupt controllers.
//!
//! The kernel takes interrupts through the local APIC only. The PICs still
//! exist on PC-compatible hardware and come out of reset mapped onto the CPU
//! exception vectors, so they are moved out of the way and fully masked.

use super::interrupts::FIRST_VECTOR;
use super::port::{inb, outb};

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xA0;
const SLAVE_DATA: u16 = 0xA1;

/// Remaps both PICs to vectors 32-47 and masks all sixteen lines.
pub fn disable() {
    // SAFETY: ports 0x20/0x21/0xA0/0xA1 belong to the 8259 pair on
    // PC-compatible hardware. This is the standard initialisation sequence
    // (ICW1-ICW4) followed by a full mask; it only affects interrupt
    // routing, and interrupts are disabled while it runs. On hardware
    // without a PIC the writes are ignored. The dummy reads give slow
    // controllers time to settle between command words.
    unsafe {
        outb(MASTER_COMMAND, 0x11); // ICW1: initialise, expect ICW4
        outb(SLAVE_COMMAND, 0x11);
        inb(MASTER_DATA);
        outb(MASTER_DATA, FIRST_VECTOR); // ICW2: vector base
        outb(SLAVE_DATA, FIRST_VECTOR + 8);
        inb(MASTER_DATA);
        outb(MASTER_DATA, 0x04); // ICW3: slave on IRQ2
        outb(SLAVE_DATA, 0x02);
        inb(MASTER_DATA);
        outb(MASTER_DATA, 0x01); // ICW4: 8086 mode
        outb(SLAVE_DATA, 0x01);
        inb(MASTER_DATA);
        outb(MASTER_DATA, 0xFF); // mask everything
        outb(SLAVE_DATA, 0xFF);
    }
}
