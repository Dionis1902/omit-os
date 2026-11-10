// SPDX-License-Identifier: GPL-2.0-only
//! The on-screen status bar.
//!
//! One line at the top of the framebuffer console showing live system
//! state, refreshed about once a second while the system waits for input.

use alloc::format;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::mm::{self, PAGE_SIZE};
use crate::{arch, fbcon, time};

/// Uptime, in seconds, at which the bar was last drawn; `u64::MAX` before
/// the first draw.
static LAST_REFRESH: AtomicU64 = AtomicU64::new(u64::MAX);

/// Redraws the status bar if the displayed second has changed. Cheap to
/// call often.
pub fn refresh_status() {
    let seconds = time::monotonic_ns() / 1_000_000_000;
    if LAST_REFRESH.swap(seconds, Ordering::Relaxed) == seconds {
        return;
    }

    let (free, managed) = mm::frame::stats().map_or((0, 0), |stats| {
        let mib = |frames: usize| (frames as u64 * PAGE_SIZE) >> 20;
        (mib(stats.free), mib(stats.managed))
    });
    let heap_kib = mm::heap::stats().allocated_bytes / 1024;
    fbcon::set_status(&format!(
        " Omit OS {} | {} | up {}:{:02}:{:02} | memory {} of {} MiB free | kernel heap {} KiB",
        env!("CARGO_PKG_VERSION"),
        arch::NAME,
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60,
        free,
        managed,
        heap_kib,
    ));
}
