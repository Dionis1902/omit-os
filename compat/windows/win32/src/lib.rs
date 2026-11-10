// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
// Sources: see the modules; each lists its own.
//! The parts of the Windows API layer that do not depend on the machine
//! they run on, kept separate so that they can be tested on the build
//! host: error codes, translation of paths and command lines, text
//! conversion and the handle table.
//!
//! Everything here is written from the public Windows API documentation
//! (Microsoft Learn); the numeric constants are the documented values that
//! programs are compiled against. There is no Windows code in it.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod cmdline;
pub mod error;
pub mod handle;
pub mod path;
pub mod pattern;
pub mod text;
