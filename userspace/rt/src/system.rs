// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the system reports about itself: memory, uptime, and the
//! threads that exist.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use omit_abi::nr;
pub use omit_abi::system::{
    Info, STATE_BLOCKED, STATE_DEAD, STATE_READY, STATE_RUNNING, TASK_IDLE, TASKS_MAX,
    Task as RawTask,
};

use crate::syscall;

/// Memory, uptime and idle time at this moment.
pub fn info() -> Info {
    let mut info = Info::default();
    // SAFETY: the pointer refers to a live, writable `Info`.
    unsafe { syscall(nr::SYSTEM_INFO, &raw mut info as u64, 0, 0) };
    info
}

/// One thread of the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// The thread's ID.
    pub thread: u64,
    /// The process it belongs to; `None` for a thread of the kernel.
    pub pid: Option<u64>,
    /// Milliseconds of processor time used so far.
    pub cpu_ms: u64,
    /// One of the `STATE_*` values.
    pub state: u32,
    /// Its name; for a process, the program.
    pub name: String,
    /// It is the thread that runs when no other can.
    pub idle: bool,
}

/// Every thread in the system, ordered by thread ID. (If there are more
/// than [`TASKS_MAX`], the first that many.)
pub fn tasks() -> Vec<Task> {
    let mut raw = vec![RawTask::default(); 64];
    loop {
        // SAFETY: `raw` is a live, writable array of `raw.len()` entries.
        let count = unsafe { syscall(nr::TASK_LIST, raw.as_mut_ptr() as u64, raw.len() as u64, 0) };
        let Ok(count) = usize::try_from(count) else {
            return Vec::new();
        };
        if count > raw.len() && raw.len() < TASKS_MAX as usize {
            // More threads than there was room for: ask again.
            raw.resize(TASKS_MAX as usize, RawTask::default());
            continue;
        }
        raw.truncate(count);
        break;
    }
    raw.iter()
        .map(|task| Task {
            thread: task.thread,
            pid: (task.pid != 0).then_some(task.pid),
            cpu_ms: task.cpu_ms,
            state: task.state,
            idle: task.flags & TASK_IDLE != 0,
            name: String::from_utf8_lossy(
                &task.name[..(task.name_len as usize).min(task.name.len())],
            )
            .into_owned(),
        })
        .collect()
}
