// SPDX-License-Identifier: GPL-2.0-only
//! System call handlers.
//!
//! The ABI (numbers, argument registers, error values) is defined in the
//! `omit_abi` crate. Everything a user program passes in is untrusted:
//! numbers are range-checked and pointers are only ever used through the
//! process's address space, which validates them page by page.

use omit_abi::{Errno, STDERR, STDOUT, WRITE_MAX, nr};

use crate::{kprint, process, sched, time};

/// Entry point for all system calls; called by the assembly entry code
/// with interrupts enabled, on the calling thread's kernel stack.
pub extern "C" fn dispatch(number: u64, a0: u64, a1: u64, a2: u64, _a3: u64, _a4: u64) -> i64 {
    process::exit_if_killed();
    let result = match number {
        nr::EXIT => process::exit(a0 as i32),
        nr::WRITE => write(a0, a1, a2),
        nr::GETPID => sched::current_pid().map_or(0, |pid| pid as i64),
        nr::YIELD => {
            sched::yield_now();
            0
        }
        nr::SLEEP => {
            // Round up so a sleep is never shorter than requested.
            let ticks = a0.saturating_mul(u64::from(time::TICK_HZ)).div_ceil(1000);
            sched::sleep_ticks(ticks);
            0
        }
        nr::UPTIME => (time::monotonic_ns() / 1_000_000) as i64,
        _ => Errno::NoSuchSyscall.as_result(),
    };
    process::exit_if_killed();
    result
}

fn write(fd: u64, address: u64, length: u64) -> i64 {
    if fd != STDOUT && fd != STDERR {
        return Errno::BadFileDescriptor.as_result();
    }
    if length > WRITE_MAX {
        return Errno::InvalidArgument.as_result();
    }

    // Copy in bounded pieces so the scheduler lock is held only briefly and
    // the kernel stack use is fixed.
    let mut buffer = [0u8; 256];
    let mut done = 0u64;
    while done < length {
        let chunk = (length - done).min(buffer.len() as u64) as usize;
        let copied =
            sched::with_address_space(|space| space.read(address + done, &mut buffer[..chunk]));
        if copied != Some(true) {
            // Nothing from a bad buffer is printed, but earlier chunks
            // already were; report the fault either way.
            return Errno::Fault.as_result();
        }
        // Program output is arbitrary bytes; show what is valid text.
        for piece in buffer[..chunk].utf8_chunks() {
            kprint!("{}", piece.valid());
            if !piece.invalid().is_empty() {
                kprint!("\u{fffd}");
            }
        }
        done += chunk as u64;
    }
    length as i64
}
