// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! The kernel's root filesystem.
//!
//! One [`Vfs`] instance shared by the shell and by the file system calls of
//! user processes, behind a sleeping mutex: a filesystem operation may take
//! a while, and other threads keep running in the meantime.

use alloc::boxed::Box;

use omit_vfs::ramfs::RamFs;
use omit_vfs::{Vfs, tar};

use crate::sync::{Mutex, MutexGuard};
use crate::{info, warn};

static ROOT: Mutex<Option<Vfs>> = Mutex::new(None);

/// Creates the root filesystem (a RAM filesystem) and unpacks the initial
/// ramdisk archive into it. Requires the scheduler to be running.
pub fn init(initrd: &'static [u8]) {
    *INITRD.lock() = initrd;
    let mut vfs = Vfs::new(Box::new(RamFs::new()));
    info!("vfs: RAM filesystem mounted at /");
    if !initrd.is_empty() {
        match tar::unpack(&mut vfs, initrd) {
            Ok(files) => info!(
                "vfs: unpacked {files} file(s) from the initial ramdisk ({} KiB)",
                initrd.len() / 1024
            ),
            Err(error) => warn!("vfs: initial ramdisk is damaged: {error}"),
        }
    }
    *ROOT.lock() = Some(vfs);
}

/// The initial ramdisk archive as the boot loader provided it. The
/// installer copies it to the installed system unchanged.
static INITRD: crate::sync::SpinLock<&'static [u8]> = crate::sync::SpinLock::new(&[]);

/// The initial ramdisk archive this system was booted with (empty if
/// there was none).
pub fn initrd() -> &'static [u8] {
    *INITRD.lock()
}

/// Exclusive access to the root filesystem.
pub struct RootGuard(MutexGuard<'static, Option<Vfs>>);

impl core::ops::Deref for RootGuard {
    type Target = Vfs;

    fn deref(&self) -> &Vfs {
        self.0.as_ref().expect("checked in `root`")
    }
}

impl core::ops::DerefMut for RootGuard {
    fn deref_mut(&mut self) -> &mut Vfs {
        self.0.as_mut().expect("checked in `root`")
    }
}

/// Locks the root filesystem. Returns `None` before [`init`].
///
/// The guard must not be held while waiting for a user process, which may
/// need the filesystem itself.
pub fn root() -> Option<RootGuard> {
    let guard = ROOT.lock();
    guard.is_some().then(|| RootGuard(guard))
}

/// Names of block devices that currently hold a mounted filesystem.
static IN_USE: crate::sync::SpinLock<alloc::vec::Vec<alloc::string::String>> =
    crate::sync::SpinLock::new(alloc::vec::Vec::new());

/// A block device seen as a byte-addressed store, for filesystem drivers.
/// While one exists, its device counts as in use.
pub struct Disk {
    name: alloc::string::String,
    size: u64,
}

impl Disk {
    /// Claims the block device `name`. Fails if it does not exist or is
    /// already claimed (for example because it is mounted).
    pub fn open(name: &str) -> Result<Self, &'static str> {
        use crate::drivers::block::{self, SECTOR_SIZE};
        let sectors = block::list()
            .into_iter()
            .find(|(device, _)| device == name)
            .map(|(_, sectors)| sectors)
            .ok_or("no such block device")?;
        // A device shares its storage with its partitions, and a
        // partition with its whole device.
        let related = block::related(name);
        let mut in_use = IN_USE.lock();
        if in_use.iter().any(|device| related.contains(device)) {
            return Err("device is in use");
        }
        in_use.push(name.into());
        Ok(Self {
            name: name.into(),
            size: sectors * SECTOR_SIZE as u64,
        })
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        IN_USE.lock().retain(|device| *device != self.name);
    }
}

impl omit_ext2::BlockIo for Disk {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), omit_ext2::IoError> {
        use crate::drivers::block::{self, SECTOR_SIZE};
        // Filesystem drivers only issue sector-aligned requests.
        if !offset.is_multiple_of(SECTOR_SIZE as u64) || !buffer.len().is_multiple_of(SECTOR_SIZE) {
            return Err(omit_ext2::IoError);
        }
        block::with_device(&self.name, |device| {
            device.read(offset / SECTOR_SIZE as u64, buffer)
        })
        .map_err(|_| omit_ext2::IoError)
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), omit_ext2::IoError> {
        use crate::drivers::block::{self, SECTOR_SIZE};
        if !offset.is_multiple_of(SECTOR_SIZE as u64) || !data.len().is_multiple_of(SECTOR_SIZE) {
            return Err(omit_ext2::IoError);
        }
        block::with_device(&self.name, |device| {
            device.write(offset / SECTOR_SIZE as u64, data)
        })
        .map_err(|_| omit_ext2::IoError)
    }
}
