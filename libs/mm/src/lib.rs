// SPDX-License-Identifier: GPL-2.0-only
//! Architecture-independent memory management algorithms.
//!
//! Everything in this crate operates on caller-provided storage or a
//! caller-provided page source, so it is unit-tested on the host and reused
//! unchanged by the kernel.
//!
//! `unsafe` is denied crate-wide and allowed only in [`heap`], whose job
//! (handing out raw memory) cannot be expressed without it.

#![no_std]
#![deny(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod buddy;
#[allow(unsafe_code)]
pub mod heap;

pub use buddy::{Buddy, BuddyError, FrameMeta, MAX_ORDER};
pub use heap::{Heap, HeapStats, PageSource};
