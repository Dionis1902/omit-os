// SPDX-License-Identifier: GPL-2.0-only
//! Block devices.
//!
//! A registry of storage devices addressed in 512-byte sectors. There is
//! one synchronous interface: read or write a run of sectors and return
//! when the device has completed it.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::sync::Mutex;

/// Size of a sector, the unit all block devices are addressed in.
pub const SECTOR_SIZE: usize = 512;

/// A failed block operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// The request extends past the end of the device, or the buffer is
    /// not a whole number of sectors.
    OutOfRange,
    /// The device reported an error.
    Io,
    /// The device did not answer in time.
    Timeout,
    /// There is no such device.
    NoDevice,
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfRange => "request outside the device",
            Self::Io => "device reported an I/O error",
            Self::Timeout => "device timed out",
            Self::NoDevice => "no such block device",
        })
    }
}

/// A storage device.
pub trait BlockDevice: Send {
    /// Number of sectors.
    fn sectors(&self) -> u64;

    /// Reads `buffer.len() / 512` sectors starting at `sector`.
    fn read(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError>;

    /// Writes `data.len() / 512` sectors starting at `sector`. The data
    /// has reached the device when this returns.
    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), BlockError>;
}

struct Entry {
    name: String,
    device: Box<dyn BlockDevice>,
}

static DEVICES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Registers `device` under `name` (for example `vda`).
pub fn register(name: &str, device: Box<dyn BlockDevice>) {
    DEVICES.lock().push(Entry {
        name: name.into(),
        device,
    });
}

/// Names and sizes (in sectors) of all registered devices.
pub fn list() -> Vec<(String, u64)> {
    DEVICES
        .lock()
        .iter()
        .map(|entry| (entry.name.clone(), entry.device.sectors()))
        .collect()
}

/// Runs `f` on the device called `name`, with exclusive access to all
/// block devices for the duration.
pub fn with_device<R>(
    name: &str,
    f: impl FnOnce(&mut dyn BlockDevice) -> Result<R, BlockError>,
) -> Result<R, BlockError> {
    let mut devices = DEVICES.lock();
    let entry = devices
        .iter_mut()
        .find(|entry| entry.name == name)
        .ok_or(BlockError::NoDevice)?;
    f(entry.device.as_mut())
}
