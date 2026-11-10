// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Shared memory segments.
//!
//! A segment is a set of page frames that several processes can map at
//! the same time: what one writes, the others see, without a system
//! call. It is how a program hands the pixels of its window to the
//! desktop.
//!
//! A segment is named by a token, a 64-bit number chosen by the kernel.
//! Whoever knows the token can map the segment, so a program shares a
//! segment by telling another program the token (over a channel, say).
//! Tokens are hard to guess but are **not** a security boundary: they
//! come from a simple generator seeded by the clock.
//!
//! A segment lives as long as it is mapped somewhere; each mapping holds
//! a reference (see `process::map_shared`).

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use omit_abi::SHARED_MAX;

use crate::mm::{PAGE_SIZE, frame};
use crate::sync::SpinLock;

/// The frames of one segment.
pub struct Segment {
    token: u64,
    frames: Vec<u64>,
}

impl Segment {
    /// Physical addresses of the segment's frames, in order.
    pub fn frames(&self) -> &[u64] {
        &self.frames
    }

    /// Size in bytes.
    pub fn len(&self) -> u64 {
        self.frames.len() as u64 * PAGE_SIZE
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        for &phys in &self.frames {
            // SAFETY: every mapping of the segment held a reference; this
            // was the last one, so no page table refers to the frames.
            let _ = unsafe { frame::free(phys, 0) };
        }
        let mut segments = SEGMENTS.lock();
        // Unless the token has been reused already (it never is).
        if segments
            .get(&self.token)
            .is_some_and(|weak| weak.strong_count() == 0)
        {
            segments.remove(&self.token);
        }
    }
}

static SEGMENTS: SpinLock<BTreeMap<u64, Weak<Segment>>> = SpinLock::new(BTreeMap::new());
static GENERATOR: AtomicU64 = AtomicU64::new(0);

/// The next token: a counter run through a mixing function (SplitMix64),
/// started from the time of first use.
fn next_token() -> u64 {
    let _ = GENERATOR.compare_exchange(
        0,
        crate::time::monotonic_ns() | 1,
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
    let mut z = GENERATOR.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Creates a zero-filled segment of `length` bytes, rounded up to whole
/// pages. `None` if the length is 0 or too large, or memory is short.
pub fn create(length: u64) -> Option<Arc<Segment>> {
    if length == 0 || length > SHARED_MAX {
        return None;
    }
    let pages = length.div_ceil(PAGE_SIZE) as usize;
    let mut segment = Segment {
        token: 0,
        frames: Vec::new(),
    };
    segment.frames.try_reserve_exact(pages).ok()?;
    for _ in 0..pages {
        // On failure, dropping `segment` frees what was allocated.
        segment.frames.push(frame::alloc_zeroed(0)?);
    }
    let mut segments = SEGMENTS.lock();
    segment.token = loop {
        let token = next_token();
        if token != 0 && !segments.contains_key(&token) {
            break token;
        }
    };
    let segment = Arc::new(segment);
    segments.insert(segment.token, Arc::downgrade(&segment));
    Some(segment)
}

impl Segment {
    /// The token that names the segment.
    pub fn token(&self) -> u64 {
        self.token
    }
}

/// The segment named by `token`, if it still exists.
pub fn find(token: u64) -> Option<Arc<Segment>> {
    // The reference is created under the lock but never dropped under
    // it: `Segment::drop` takes the lock itself.
    let segments = SEGMENTS.lock();
    let found = segments.get(&token).and_then(Weak::upgrade);
    drop(segments);
    found
}
