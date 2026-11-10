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
pub fn init(initrd: &[u8]) {
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
