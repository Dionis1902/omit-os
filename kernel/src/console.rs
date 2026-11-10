// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Console input.
//!
//! Bytes received from the serial port are queued by the interrupt handler
//! and consumed by [`read_byte`], which sleeps the CPU while the queue is
//! empty. Output goes through [`crate::log`].

use core::fmt;

use crate::arch::serial;
use crate::sched::WaitQueue;
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
    INPUT_AVAILABLE.wake_all();
}

/// Threads blocked in [`read_byte`].
static INPUT_AVAILABLE: WaitQueue = WaitQueue::new();

/// Starts queueing serial input. The serial interrupt must be routed to
/// [`serial::handle_interrupt`] separately.
pub fn enable_input() {
    serial::enable_receive_interrupt(push_input);
}

/// Blocks until a byte of console input is available and returns it.
///
/// The calling thread sleeps while the queue is empty; other threads (or
/// the idle thread) run in the meantime.
pub fn read_byte() -> u8 {
    let mut byte = None;
    INPUT_AVAILABLE.wait_until(|| {
        byte = INPUT.lock().pop();
        byte.is_some()
    });
    byte.expect("wait_until returns only once the condition holds")
}

/// Like [`read_byte`], but gives up and returns `None` if the calling
/// process is killed while waiting.
pub fn read_byte_unless_killed() -> Option<u8> {
    let mut byte = None;
    INPUT_AVAILABLE.wait_until(|| {
        byte = INPUT.lock().pop();
        byte.is_some() || crate::sched::current_killed()
    });
    byte
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
