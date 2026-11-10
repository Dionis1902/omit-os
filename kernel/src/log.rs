// SPDX-License-Identifier: GPL-2.0-only
//! Kernel console output and diagnostic logging.
//!
//! Output goes to the first serial port and, once it is initialised, to the
//! framebuffer console. Lines produced by the
//! level macros have the fixed shape `[LEVEL] message`, which is what the
//! automated boot tests parse.

use core::fmt::{self, Write};

use crate::arch::serial;
use crate::sync::SpinLock;

/// Console output produced before the framebuffer console exists, kept so
/// it can be shown on screen once it does. When the buffer is full, later
/// output is not recorded (it still reaches the serial port).
struct EarlyLog {
    bytes: [u8; EARLY_LOG_CAPACITY],
    len: usize,
    /// Cleared once the log has been replayed.
    recording: bool,
}

const EARLY_LOG_CAPACITY: usize = 8192;

static EARLY_LOG: SpinLock<EarlyLog> = SpinLock::new(EarlyLog {
    bytes: [0; EARLY_LOG_CAPACITY],
    len: 0,
    recording: true,
});

impl fmt::Write for EarlyLog {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        // Only whole strings are stored, so the buffer stays valid UTF-8.
        if self.recording && text.len() <= EARLY_LOG_CAPACITY - self.len {
            self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
            self.len += text.len();
        }
        Ok(())
    }
}

/// Passes the recorded early output to `show` and stops recording.
pub fn replay_early_log(show: impl FnOnce(&str)) {
    let mut log = EARLY_LOG.lock();
    log.recording = false;
    if let Ok(text) = core::str::from_utf8(&log.bytes[..log.len]) {
        show(text);
    }
}

/// Writes formatted text to the kernel console. Use the macros instead.
pub fn write_fmt(args: fmt::Arguments<'_>) {
    // Formatting into the serial port cannot fail; a `fmt::Error` could only
    // come from a `Display` impl, and there is nowhere to report it.
    let _ = serial::console().write_fmt(args);
    if let Some(mut early) = EARLY_LOG.try_lock()
        && early.recording
    {
        let _ = early.write_fmt(args);
        return;
    }
    crate::fbcon::write_fmt(args);
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
