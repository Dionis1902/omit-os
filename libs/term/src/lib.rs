// SPDX-License-Identifier: GPL-2.0-only
//! Text terminal emulation and keyboard decoding.
//!
//! * [`screen`] turns a stream of characters and control sequences into a
//!   grid of character cells and tells a [`screen::Display`] which cells to
//!   redraw. It knows nothing about pixels.
//! * [`scancode`] turns PC keyboard scancodes into key presses.
//! * [`mouse`] turns the byte stream of a PS/2 mouse into movements.
//!
//! Both are pure state machines with no `unsafe` code, unit-tested on the
//! host; the kernel supplies the pixel drawing and the hardware access.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod mouse;
pub mod scancode;
pub mod screen;
