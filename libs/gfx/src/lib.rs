// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Graphics for Omit OS programs.
//!
//! * [`geometry`] — points and rectangles.
//! * [`surface`] — an image in memory and the basic operations that draw
//!   on it, always clipped: filled rectangles, outlines, fixed-width text.
//! * [`paint`] — drawing with soft edges: translucency, rounded
//!   rectangles, shadows, proportional text, icons.
//! * [`font`], [`icon`] — the Inter typeface and the interface's icons,
//!   ready-drawn.
//! * [`theme`] — the colours of the Omit OS design system, light and
//!   dark.
//! * [`desktop`] — a window manager drawn to that design system:
//!   overlapping windows that can be moved, resized, minimised and
//!   maximised, a task bar, a Start panel and a pointer. What is *in* a
//!   window is provided by the program through the [`desktop::App`]
//!   trait.
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

pub mod art;
pub mod desktop;
pub mod files;
pub mod font;
pub mod geometry;
pub mod icon;
pub mod paint;
pub mod settings;
pub mod surface;
pub mod theme;
pub mod widgets;

/// A colour: `0x00RRGGBB`.
pub type Colour = u32;
