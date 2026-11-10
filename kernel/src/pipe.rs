// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Pipes: one-way byte streams between processes.
//!
//! A pipe is a bounded buffer with a reading end and a writing end. Each
//! end can be held by several descriptors (a program and the programs it
//! starts, for instance). Reading blocks while the buffer is empty and
//! returns 0 — end of input — once it is empty and no writing end is left.
//! Writing blocks while the buffer is full and fails once no reading end
//! is left, so a producer does not run on with nobody listening.

use alloc::collections::VecDeque;
use alloc::sync::Arc;

use crate::sched::{self, WaitQueue};
use crate::sync::SpinLock;

/// Bytes a pipe holds before writers have to wait.
pub const CAPACITY: usize = 64 * 1024;

struct State {
    buffer: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

struct Shared {
    state: SpinLock<State>,
    /// Woken whenever data arrives, space appears or an end goes away.
    changed: WaitQueue,
}

impl Shared {
    /// Wakes the threads blocked on this pipe and those in `poll`.
    fn wake(&self) {
        self.changed.wake_all();
        crate::poll::notify();
    }
}

/// The reading end of a pipe.
pub struct Reader(Arc<Shared>);

/// The writing end of a pipe.
pub struct Writer(Arc<Shared>);

/// Why a pipe operation did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeError {
    /// Every reading end has been closed.
    NoReaders,
    /// The calling thread was killed while waiting.
    Killed,
}

/// Creates a pipe and returns its two ends.
pub fn new() -> (Reader, Writer) {
    let shared = Arc::new(Shared {
        state: SpinLock::new(State {
            buffer: VecDeque::new(),
            readers: 1,
            writers: 1,
        }),
        changed: WaitQueue::new(),
    });
    (Reader(shared.clone()), Writer(shared))
}

impl Reader {
    /// Reads up to `buffer.len()` bytes, waiting for at least one. Returns
    /// 0 at end of input: the pipe is empty and no writing end exists.
    pub fn read(&self, buffer: &mut [u8]) -> Result<usize, PipeError> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut state = self.0.state.lock();
                if !state.buffer.is_empty() {
                    let count = buffer.len().min(state.buffer.len());
                    for (slot, byte) in buffer.iter_mut().zip(state.buffer.drain(..count)) {
                        *slot = byte;
                    }
                    drop(state);
                    // Writers may be waiting for this space.
                    self.0.wake();
                    return Ok(count);
                }
                if state.writers == 0 {
                    return Ok(0);
                }
            }
            if sched::current_killed() {
                return Err(PipeError::Killed);
            }
            self.0.changed.wait_until(|| {
                let state = self.0.state.lock();
                !state.buffer.is_empty() || state.writers == 0 || sched::current_killed()
            });
        }
    }
}

impl Reader {
    /// Like [`Reader::read`], but never waits: `None` means the pipe is
    /// empty and a writing end still exists.
    pub fn try_read(&self, buffer: &mut [u8]) -> Option<usize> {
        let mut state = self.0.state.lock();
        if state.buffer.is_empty() {
            return (state.writers == 0 || buffer.is_empty()).then_some(0);
        }
        let count = buffer.len().min(state.buffer.len());
        for (slot, byte) in buffer.iter_mut().zip(state.buffer.drain(..count)) {
            *slot = byte;
        }
        drop(state);
        self.0.wake();
        Some(count)
    }
}

impl Reader {
    /// Whether a read would return at once: there is data, or no writing
    /// end is left.
    pub fn ready(&self) -> bool {
        let state = self.0.state.lock();
        !state.buffer.is_empty() || state.writers == 0
    }
}

impl Writer {
    /// Like [`Writer::write`], but never waits: if there is not room for
    /// all of `data`, nothing is written and `Ok(false)` returned. (Data
    /// longer than [`CAPACITY`] can therefore never be written this way.)
    pub fn try_write(&self, data: &[u8]) -> Result<bool, PipeError> {
        let mut state = self.0.state.lock();
        if state.readers == 0 {
            return Err(PipeError::NoReaders);
        }
        if CAPACITY - state.buffer.len() < data.len() {
            return Ok(false);
        }
        state.buffer.extend(data);
        drop(state);
        self.0.wake();
        Ok(true)
    }
}

impl Writer {
    /// Writes all of `data`, waiting for space as needed.
    pub fn write(&self, mut data: &[u8]) -> Result<(), PipeError> {
        while !data.is_empty() {
            {
                let mut state = self.0.state.lock();
                if state.readers == 0 {
                    return Err(PipeError::NoReaders);
                }
                let space = CAPACITY - state.buffer.len();
                if space != 0 {
                    let count = space.min(data.len());
                    state.buffer.extend(&data[..count]);
                    data = &data[count..];
                    drop(state);
                    self.0.wake();
                    continue;
                }
            }
            if sched::current_killed() {
                return Err(PipeError::Killed);
            }
            self.0.changed.wait_until(|| {
                let state = self.0.state.lock();
                state.buffer.len() < CAPACITY || state.readers == 0 || sched::current_killed()
            });
        }
        Ok(())
    }
}

impl Clone for Reader {
    fn clone(&self) -> Self {
        self.0.state.lock().readers += 1;
        Self(self.0.clone())
    }
}

impl Clone for Writer {
    fn clone(&self) -> Self {
        self.0.state.lock().writers += 1;
        Self(self.0.clone())
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.0.state.lock().readers -= 1;
        // Writers must learn that nobody may be listening any more.
        self.0.wake();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.0.state.lock().writers -= 1;
        // Readers must learn that the input may have ended.
        self.0.wake();
    }
}

/// Exercises a pipe within one thread and across two.
pub fn self_test() -> Result<(), &'static str> {
    let (reader, writer) = new();
    writer
        .write(b"hello")
        .map_err(|_| "write to a fresh pipe failed")?;
    let mut buffer = [0u8; 16];
    if reader.read(&mut buffer[..3]) != Ok(3) || &buffer[..3] != b"hel" {
        return Err("partial read returned the wrong data");
    }
    if reader.read(&mut buffer) != Ok(2) || &buffer[..2] != b"lo" {
        return Err("second read returned the wrong data");
    }
    // A second writing end keeps the pipe open until both are gone.
    let second = writer.clone();
    drop(writer);
    second
        .write(b"x")
        .map_err(|_| "write through a cloned end failed")?;
    drop(second);
    if reader.read(&mut buffer) != Ok(1) || reader.read(&mut buffer) != Ok(0) {
        return Err("end of input was not reported after the last writer closed");
    }
    let (reader, writer) = new();
    drop(reader);
    if writer.write(b"y") != Err(PipeError::NoReaders) {
        return Err("writing without readers did not fail");
    }

    // More data than the pipe holds, from another thread: the writer has
    // to block and be woken by the reader, and the data must arrive in
    // order and complete.
    const TOTAL: usize = 3 * CAPACITY + 1234;
    static WRITER_SLOT: SpinLock<Option<Writer>> = SpinLock::new(None);
    fn producer(_: usize) {
        let Some(writer) = WRITER_SLOT.lock().take() else {
            return;
        };
        let mut sent = 0usize;
        let mut chunk = [0u8; 4096];
        while sent < TOTAL {
            let count = chunk.len().min(TOTAL - sent);
            for (offset, byte) in chunk[..count].iter_mut().enumerate() {
                *byte = ((sent + offset) % 251) as u8;
            }
            if writer.write(&chunk[..count]).is_err() {
                return;
            }
            sent += count;
        }
    }
    let (reader, writer) = new();
    *WRITER_SLOT.lock() = Some(writer);
    let thread = sched::spawn("pipe-test", producer, 0).map_err(|_| "cannot spawn a thread")?;
    let mut received = 0usize;
    let mut chunk = [0u8; 1000];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                for (offset, &byte) in chunk[..count].iter().enumerate() {
                    if byte != ((received + offset) % 251) as u8 {
                        return Err("data arrived out of order through a pipe");
                    }
                }
                received += count;
            }
            Err(_) => return Err("reading from a pipe failed"),
        }
    }
    sched::join(thread);
    if received != TOTAL {
        return Err("data was lost in a pipe");
    }
    Ok(())
}
