// SPDX-License-Identifier: GPL-2.0-only
//! Kernel console output and diagnostic logging.
//!
//! All output currently goes to the first serial port. Lines produced by the
//! level macros have the fixed shape `[LEVEL] message`, which is what the
//! automated boot tests parse.

use core::fmt::{self, Write};

use crate::arch::serial;

/// Writes formatted text to the kernel console. Use the macros instead.
pub fn write_fmt(args: fmt::Arguments<'_>) {
    // Formatting into the serial port cannot fail; a `fmt::Error` could only
    // come from a `Display` impl, and there is nowhere to report it.
    let _ = serial::console().write_fmt(args);
}

/// Prints to the kernel console without a trailing newline.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::log::write_fmt(format_args!($($arg)*))
    };
}

/// Prints a line to the kernel console.
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => {
        $crate::log::write_fmt(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// Logs an informational message.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::kprintln!("[INFO ] {}", format_args!($($arg)*)) };
}

/// Logs a warning: something unexpected that the kernel can continue past.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::kprintln!("[WARN ] {}", format_args!($($arg)*)) };
}

/// Logs an error: an operation failed.
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::kprintln!("[ERROR] {}", format_args!($($arg)*)) };
}
