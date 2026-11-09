// SPDX-License-Identifier: GPL-2.0-only
//! 8254 programmable interval timer.
//!
//! Used only as a reference clock of known frequency for calibrating the TSC
//! and the local APIC timer. Channel 2 (the "speaker" channel) is used
//! because its output can be polled through port 0x61 without involving
//! interrupts.

use super::port::{inb, outb};

/// Input clock of the PIT in Hz.
pub const FREQUENCY_HZ: u32 = 1_193_182;

const CHANNEL_2_DATA: u16 = 0x42;
const COMMAND: u16 = 0x43;
const SPEAKER_CONTROL: u16 = 0x61;

const GATE_2: u8 = 1 << 0;
const SPEAKER_ENABLE: u8 = 1 << 1;
const OUT_2: u8 = 1 << 5;

/// Bound on polls in [`wait_for_one_shot`], so that a machine without a PIT
/// produces an error instead of a hang. Each poll is a port read (roughly a
/// microsecond on hardware), so this is far beyond the 55 ms maximum period.
const POLL_LIMIT: u32 = 50_000_000;

/// Starts channel 2 counting down from `ticks` at [`FREQUENCY_HZ`].
pub fn start_one_shot(ticks: u16) {
    let [low, high] = ticks.to_le_bytes();
    // SAFETY: ports 0x42, 0x43 and 0x61 are the PIT channel 2 data port, the
    // PIT command port and the NMI status/speaker control port on
    // PC-compatible hardware. The sequence gates channel 2 on with the
    // speaker disconnected and programs mode 0 (output goes high when the
    // count expires); it affects no other device.
    unsafe {
        let control = inb(SPEAKER_CONTROL);
        outb(SPEAKER_CONTROL, (control & !SPEAKER_ENABLE) | GATE_2);
        outb(COMMAND, 0b1011_0000); // channel 2, low/high byte, mode 0
        outb(CHANNEL_2_DATA, low);
        outb(CHANNEL_2_DATA, high); // counting starts here
    }
}

/// Busy-waits until the count started by [`start_one_shot`] expires.
/// Returns `false` if it never does.
pub fn wait_for_one_shot() -> bool {
    for _ in 0..POLL_LIMIT {
        // SAFETY: reading port 0x61 has no side effects.
        if unsafe { inb(SPEAKER_CONTROL) } & OUT_2 != 0 {
            return true;
        }
    }
    false
}
