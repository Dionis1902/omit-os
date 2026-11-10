// SPDX-License-Identifier: GPL-2.0-only
//! Graphics for Omit OS programs.
//!
//! * [`geometry`] — points and rectangles.
//! * [`surface`] — an image in memory and the operations that draw on it:
//!   filled rectangles, outlines and text, always clipped.
//! * [`desktop`] — a window manager: overlapping windows with title bars
//!   that can be moved, raised and closed with the mouse, a task bar with
//!   buttons that open windows, and a pointer. What is *in* a window is
//!   provided by the program through the [`desktop::App`] trait.
//!
//! Nothing here touches hardware or makes system calls: the desktop takes
//! mouse and keyboard events as method calls and draws into a
//! [`surface::Surface`]. A program copies that surface to the screen. This
//! keeps all of the logic testable on the build host, where the tests
//! check both behaviour and the pixels drawn.
//!
//! Pixels are `u32` values holding `0x00RRGGBB`.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod desktop;
pub mod geometry;
pub mod surface;

/// A colour: `0x00RRGGBB`.
pub type Colour = u32;
