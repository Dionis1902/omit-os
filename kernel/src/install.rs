// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Installing Omit OS onto a disk.
//!
//! The installed system is the running one: the same boot loader, kernel,
//! initial ramdisk and kernel command line, written to an EFI System
//! Partition that UEFI firmware can boot from, plus a data partition with
//! an empty ext2 filesystem that the installed system mounts at `/data`.
//! The system itself still runs from memory after booting from the disk;
//! `/data` is what persists.
//!
//! Disk layout (GUID partition table):
//!
//! | partition | start | size | contents |
//! | --- | --- | --- | --- |
//! | 1 | 1 MiB | 48 MiB | EFI System Partition, FAT16 |
//! | 2 | 49 MiB | rest of the disk | Omit OS data, ext2 |
//!
//! Everything previously on the disk is lost.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use omit_gpt::{Partition, SECTOR_SIZE, TYPE_EFI_SYSTEM, TYPE_OMIT_DATA};

use crate::drivers::block::{self, BlockError};
use crate::{fs, time};

const MIB: u64 = (1 << 20) / SECTOR_SIZE;
/// Partitions start on 1 MiB boundaries, as current tools place them.
const SYSTEM_FIRST_LBA: u64 = MIB;
const SYSTEM_SECTORS: u64 = 48 * MIB;
/// Smallest disk accepted: the system partition and a useful data
/// partition.
const MIN_DISK_SECTORS: u64 = 64 * MIB;

/// The kernel command line this system was started with; the installed
/// system gets the same one.
static CMDLINE: crate::sync::SpinLock<String> = crate::sync::SpinLock::new(String::new());

/// Records the kernel command line. Called once at start-up.
pub fn set_cmdline(cmdline: &str) {
    *CMDLINE.lock() = cmdline.into();
}

/// Bytes handed to the device per write.
const WRITE_CHUNK: usize = 8 * SECTOR_SIZE as usize;

/// Identifiers for the new disk and partitions. They only have to differ
/// from those of other disks, so the clocks are entropy enough; they are
/// not secrets.
fn new_guid(salt: u64) -> [u8; 16] {
    let mut state = time::monotonic_ns() ^ crate::arch::cpu::read_tsc().rotate_left(32) ^ salt;
    let mut next = || {
        // SplitMix64.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut guid = [0u8; 16];
    guid[..8].copy_from_slice(&next().to_le_bytes());
    guid[8..].copy_from_slice(&next().to_le_bytes());
    // Mark as a random (version 4) GUID.
    guid[7] = (guid[7] & 0x0F) | 0x40;
    guid[8] = (guid[8] & 0x3F) | 0x80;
    guid
}

fn write_sectors(device: &str, first: u64, data: &[u8]) -> Result<(), BlockError> {
    block::with_device(device, |disk| {
        for (index, chunk) in data.chunks(WRITE_CHUNK).enumerate() {
            let sector = first + (index * WRITE_CHUNK) as u64 / SECTOR_SIZE;
            disk.write(sector, chunk)?;
        }
        Ok(())
    })
}

/// Installs the running system onto the whole disk `device`, erasing it.
/// `loader` and `kernel` are the boot loader and kernel files to copy;
/// the initial ramdisk and command line are those this system was started
/// with. Returns lines describing what was done.
///
/// Must not be called with the root filesystem locked by the caller's
/// caller in a way that formatting needs — it does not touch the root
/// filesystem itself.
pub fn install(device: &str, loader: &[u8], kernel: &[u8]) -> Result<Vec<String>, String> {
    let cmdline = CMDLINE.lock().clone();
    if !block::is_whole_device(device) {
        return Err(if block::list().iter().any(|(name, _)| name == device) {
            "this is a partition; install onto the whole disk".into()
        } else {
            "no such block device".into()
        });
    }
    // Claim the disk and its partitions for the duration: nothing may be
    // mounted from it.
    let claim = fs::Disk::open(device).map_err(String::from)?;

    let initrd = fs::initrd();
    if initrd.is_empty() {
        return Err("this system was started without an initial ramdisk to copy".into());
    }

    let total = block::with_device(device, |disk| Ok(disk.sectors()))
        .map_err(|error| format!("{error}"))?;
    if total < MIN_DISK_SECTORS {
        return Err(format!(
            "the disk is too small: {} MiB, and at least {} MiB are needed",
            total / MIB,
            MIN_DISK_SECTORS / MIB
        ));
    }
    let data_first = SYSTEM_FIRST_LBA + SYSTEM_SECTORS;
    // The data partition ends on a 1 MiB boundary before the backup table.
    let data_last = (omit_gpt::last_usable_lba(total) + 1) / MIB * MIB - 1;
    let partitions = [
        Partition {
            type_guid: TYPE_EFI_SYSTEM,
            unique_guid: new_guid(1),
            first_lba: SYSTEM_FIRST_LBA,
            last_lba: data_first - 1,
            name: "EFI System".into(),
        },
        Partition {
            type_guid: TYPE_OMIT_DATA,
            unique_guid: new_guid(2),
            first_lba: data_first,
            last_lba: data_last,
            name: "Omit OS data".into(),
        },
    ];
    let table = omit_gpt::build(total, new_guid(0), &partitions)
        .map_err(|error| format!("cannot lay out the disk: {error}"))?;
    let io = |error: BlockError| format!("writing to {device} failed: {error}");

    // The system partition first, the partition table last: until the
    // table is written, the disk still looks the way it did, and an
    // interrupted installation does not leave a table pointing at
    // half-written partitions.
    let mut files = alloc::vec![
        omit_fat::File {
            path: "EFI/BOOT/BOOTX64.EFI",
            data: loader,
        },
        omit_fat::File {
            path: "EFI/OMIT/KERNEL.ELF",
            data: kernel,
        },
        omit_fat::File {
            path: "EFI/OMIT/INITRD",
            data: initrd,
        },
    ];
    if !cmdline.is_empty() {
        files.push(omit_fat::File {
            path: "EFI/OMIT/CMDLINE",
            data: cmdline.as_bytes(),
        });
    }
    let mut write_error = None;
    omit_fat::format(
        SYSTEM_SECTORS as u32,
        SYSTEM_FIRST_LBA as u32,
        time::monotonic_ns() as u32,
        b"OMIT OS    ",
        &files,
        |sector, data| match write_sectors(device, SYSTEM_FIRST_LBA + u64::from(sector), data) {
            Ok(()) => true,
            Err(error) => {
                write_error = Some(error);
                false
            }
        },
    )
    .map_err(|error| match write_error {
        Some(error) => io(error),
        None => format!("cannot create the system partition: {error}"),
    })?;

    write_sectors(device, total - omit_gpt::BACK_SECTORS, &table.back).map_err(io)?;
    write_sectors(device, 0, &table.front).map_err(io)?;

    // The data partition is formatted through the partition device, which
    // exists once the new table has been read.
    drop(claim);
    let found = block::scan_partitions(device);
    if found != partitions.len() {
        return Err("the new partition table could not be read back".into());
    }
    let data_device = block::partition_name(device, 2);
    let system_device = block::partition_name(device, 1);
    let mut data = fs::Disk::open(&data_device).map_err(String::from)?;
    omit_ext2::format(&mut data)
        .map_err(|error| format!("cannot create the data filesystem: {error}"))?;
    drop(data);

    let mib = |sectors: u64| sectors / MIB;
    Ok(alloc::vec![
        format!(
            "{system_device}: EFI System Partition, {} MiB: boot loader ({} KiB), kernel ({} KiB), \
             initial ramdisk ({} KiB)",
            mib(SYSTEM_SECTORS),
            loader.len() / 1024,
            kernel.len() / 1024,
            initrd.len() / 1024
        ),
        format!(
            "{data_device}: data partition, {} MiB, empty ext2 filesystem (mounted at /data \
             when the installed system starts)",
            mib(partitions[1].sectors())
        ),
        format!("installed on {device}; remove the installation medium and restart"),
    ])
}
