// SPDX-License-Identifier: GPL-2.0-only
//! Architecture-independent memory management algorithms.
//!
//! Everything in this crate is plain safe Rust operating on caller-provided
//! storage, so it is unit-tested on the host and reused unchanged by the
//! kernel, which only supplies the memory the metadata lives in.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod buddy;

pub use buddy::{Buddy, BuddyError, FrameMeta, MAX_ORDER};
