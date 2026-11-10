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

enum Kind {
    /// A whole device, with its driver.
    Device(Box<dyn BlockDevice>),
    /// A range of sectors of the whole device called `parent`.
    Partition {
        parent: String,
        first: u64,
        sectors: u64,
        type_guid: omit_gpt::Guid,
    },
}

struct Entry {
    name: String,
    kind: Kind,
}

static DEVICES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Registers `device` under `name` (for example `vda`).
pub fn register(name: &str, device: Box<dyn BlockDevice>) {
    DEVICES.lock().push(Entry {
        name: name.into(),
        kind: Kind::Device(device),
    });
}

/// A partition seen as a device of its own: sector 0 is the partition's
/// first sector, and nothing outside it can be reached.
struct Window<'a> {
    device: &'a mut dyn BlockDevice,
    first: u64,
    sectors: u64,
}

impl Window<'_> {
    fn check(&self, sector: u64, bytes: usize) -> Result<u64, BlockError> {
        let count = (bytes / SECTOR_SIZE) as u64;
        match sector.checked_add(count) {
            Some(end) if end <= self.sectors => Ok(self.first + sector),
            _ => Err(BlockError::OutOfRange),
        }
    }
}

impl BlockDevice for Window<'_> {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, sector: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        let sector = self.check(sector, buffer.len())?;
        self.device.read(sector, buffer)
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), BlockError> {
        let sector = self.check(sector, data.len())?;
        self.device.write(sector, data)
    }
}

fn sectors_of(devices: &[Entry], index: usize) -> u64 {
    match &devices[index].kind {
        Kind::Device(device) => device.sectors(),
        Kind::Partition { sectors, .. } => *sectors,
    }
}

/// Names and sizes (in sectors) of all registered devices and partitions,
/// each device followed by its partitions.
pub fn list() -> Vec<(String, u64)> {
    let devices = DEVICES.lock();
    let mut names: Vec<(String, u64)> = (0..devices.len())
        .map(|index| (devices[index].name.clone(), sectors_of(&devices, index)))
        .collect();
    names.sort();
    names
}

/// Runs `f` on the device or partition called `name`, with exclusive
/// access to all block devices for the duration.
pub fn with_device<R>(
    name: &str,
    f: impl FnOnce(&mut dyn BlockDevice) -> Result<R, BlockError>,
) -> Result<R, BlockError> {
    let mut devices = DEVICES.lock();
    let index = devices
        .iter()
        .position(|entry| entry.name == name)
        .ok_or(BlockError::NoDevice)?;
    let (parent, first, sectors) = match &devices[index].kind {
        Kind::Device(_) => (index, 0, sectors_of(&devices, index)),
        Kind::Partition {
            parent,
            first,
            sectors,
            ..
        } => {
            let parent = devices
                .iter()
                .position(|entry| entry.name == *parent)
                .ok_or(BlockError::NoDevice)?;
            (parent, *first, *sectors)
        }
    };
    match &mut devices[parent].kind {
        Kind::Device(device) if parent == index => f(device.as_mut()),
        Kind::Device(device) => f(&mut Window {
            device: device.as_mut(),
            first,
            sectors,
        }),
        // A partition's parent is always a whole device.
        Kind::Partition { .. } => Err(BlockError::NoDevice),
    }
}

/// The device `name` together with everything that shares storage with
/// it: its partitions if it is a whole device, or the whole device if it
/// is a partition.
pub fn related(name: &str) -> Vec<String> {
    let devices = DEVICES.lock();
    let mut names = alloc::vec![String::from(name)];
    for entry in devices.iter() {
        if let Kind::Partition { parent, .. } = &entry.kind {
            if parent == name {
                names.push(entry.name.clone());
            } else if entry.name == name {
                names.push(parent.clone());
            }
        }
    }
    names
}

/// Whether `name` is a whole device (not a partition).
pub fn is_whole_device(name: &str) -> bool {
    DEVICES
        .lock()
        .iter()
        .any(|entry| entry.name == name && matches!(entry.kind, Kind::Device(_)))
}

/// Reads the partition table of the whole device `name` and registers
/// its partitions as `<name>1`, `<name>2`, ..., replacing any registered
/// before. Returns how many there are; a device without a (valid) GUID
/// partition table has none.
pub fn scan_partitions(name: &str) -> usize {
    let mut found = Vec::new();
    let result = with_device(name, |device| {
        let total = device.sectors();
        let table = omit_gpt::parse(total, |lba, buffer| device.read(lba, buffer).is_ok());
        if let Ok(partitions) = table {
            found = partitions;
        }
        Ok(())
    });
    let mut devices = DEVICES.lock();
    devices
        .retain(|entry| !matches!(&entry.kind, Kind::Partition { parent, .. } if parent == name));
    if result.is_err() {
        return 0;
    }
    for (index, partition) in found.iter().enumerate() {
        devices.push(Entry {
            name: alloc::format!("{name}{}", index + 1),
            kind: Kind::Partition {
                parent: name.into(),
                first: partition.first_lba,
                sectors: partition.sectors(),
                type_guid: partition.type_guid,
            },
        });
    }
    found.len()
}

/// The first partition marked as an Omit OS data partition, if any.
pub fn data_partition() -> Option<String> {
    let devices = DEVICES.lock();
    let mut names: Vec<&String> = devices
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                Kind::Partition { type_guid, .. } if *type_guid == omit_gpt::TYPE_OMIT_DATA
            )
        })
        .map(|entry| &entry.name)
        .collect();
    names.sort();
    names.first().map(|name| (*name).clone())
}
