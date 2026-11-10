// SPDX-License-Identifier: GPL-2.0-only
//! Console input.
//!
//! Bytes received from the serial port are queued by the interrupt handler
//! and consumed by [`read_byte`], which sleeps the CPU while the queue is
//! empty. Output goes through [`crate::log`].

use core::fmt;

use crate::arch::{interrupts, serial};
use crate::sync::SpinLock;

const CAPACITY: usize = 256;

/// Fixed-size FIFO of received bytes. When full, new bytes are dropped and
/// counted: with nobody reading, there is nothing better to do with them.
struct InputQueue {
    bytes: [u8; CAPACITY],
    head: usize,
    len: usize,
    dropped: u64,
}

impl InputQueue {
    fn push(&mut self, byte: u8) {
        if self.len == CAPACITY {
            self.dropped += 1;
            return;
        }
        self.bytes[(self.head + self.len) % CAPACITY] = byte;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        Some(byte)
    }
}

static INPUT: SpinLock<InputQueue> = SpinLock::new(InputQueue {
    bytes: [0; CAPACITY],
    head: 0,
    len: 0,
    dropped: 0,
});

/// Queues a byte of terminal input. Called from the serial and keyboard
/// interrupt handlers.
pub fn push_input(byte: u8) {
    INPUT.lock().push(byte);
}

/// Starts queueing serial input. The serial interrupt must be routed to
/// [`serial::handle_interrupt`] separately.
pub fn enable_input() {
    serial::enable_receive_interrupt(push_input);
}

/// Blocks until a byte of console input is available and returns it.
///
/// Interrupts are enabled while waiting and on return.
pub fn read_byte() -> u8 {
    loop {
        // Check with interrupts off, then enable-and-halt atomically, so an
        // interrupt arriving between the check and the halt is not missed.
        interrupts::disable();
        // The lock guard must be gone before interrupts are re-enabled.
        let byte = INPUT.lock().pop();
        if let Some(byte) = byte {
            interrupts::enable();
            return byte;
        }
        interrupts::wait();
        // The CPU wakes at least once per timer tick; use that to keep the
        // on-screen status bar current while the system is otherwise idle.
        crate::ui::refresh_status();
    }
}

/// Number of input bytes discarded because the queue was full.
pub fn dropped_input() -> u64 {
    INPUT.lock().dropped
}

/// A [`fmt::Write`] sink for the kernel console, for code that takes a
/// writer (such as the shell).
pub struct Writer;

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        crate::kprint!("{s}");
        Ok(())
    }
}
