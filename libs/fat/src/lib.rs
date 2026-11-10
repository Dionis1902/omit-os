// SPDX-License-Identifier: GPL-2.0-only
//! Creating a FAT16 filesystem that already contains a given set of
//! files.
//!
//! This is what an installer needs for an EFI System Partition: the
//! firmware reads the boot loader from a FAT filesystem, and the set of
//! files is known in advance. So instead of a general FAT driver, this
//! crate lays the whole filesystem out in one pass — boot sector, both
//! allocation tables, the root directory, then every directory and file
//! in consecutive clusters — and hands the sectors to a writer.
//!
//! Written from Microsoft's FAT specification ("FAT: General Overview of
//! On-Disk Format"). Restrictions, all checked: file and directory names
//! must be plain 8.3 names (no long names are written), the volume must
//! be of a size FAT16 can describe (about 2 MiB to 2 GiB with the cluster
//! sizes used here), and everything must fit. Nothing is read: an
//! existing filesystem on the target is simply replaced.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

use alloc::vec;
use alloc::vec::Vec;

/// Bytes per sector.
pub const SECTOR_SIZE: usize = 512;

const RESERVED_SECTORS: u32 = 1;
const FAT_COPIES: u32 = 2;
const ROOT_ENTRIES: u32 = 512;
const ROOT_SECTORS: u32 = ROOT_ENTRIES * 32 / SECTOR_SIZE as u32;
const ENTRY_SIZE: usize = 32;
/// FAT16 is defined by the cluster count lying in this range.
const MIN_CLUSTERS: u32 = 4085;
const MAX_CLUSTERS: u32 = 65524;
const END_OF_CHAIN: u16 = 0xFFFF;
const MEDIA_FIXED_DISK: u8 = 0xF8;

const ATTR_VOLUME_LABEL: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;

/// Timestamp given to everything: 2026-01-01 00:00. Fixed, so that the
/// same input always produces the same filesystem.
const DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;

/// A file to put on the volume.
#[derive(Debug, Clone, Copy)]
pub struct File<'a> {
    /// Path from the root with `/` between components, such as
    /// `EFI/BOOT/BOOTX64.EFI`. Directories are created as needed.
    pub path: &'a str,
    /// The file's contents.
    pub data: &'a [u8],
}

/// Why the filesystem could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatError {
    /// The volume is too small or too large for FAT16.
    VolumeSize,
    /// A path component is not a valid 8.3 name.
    BadName,
    /// The same path appears twice, or a file where a directory is needed.
    Conflict,
    /// The root directory would need more than 512 entries.
    RootDirectoryFull,
    /// The files do not fit on the volume.
    NoSpace,
    /// The writer reported a failure.
    Io,
}

impl core::fmt::Display for FatError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::VolumeSize => "volume size not supported by FAT16",
            Self::BadName => "file name is not a valid 8.3 name",
            Self::Conflict => "conflicting paths",
            Self::RootDirectoryFull => "too many entries in the root directory",
            Self::NoSpace => "files do not fit on the volume",
            Self::Io => "write error",
        })
    }
}

/// Where everything is on a volume of a given size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    total_sectors: u32,
    sectors_per_cluster: u32,
    fat_sectors: u32,
    clusters: u32,
}

impl Geometry {
    fn new(total_sectors: u32) -> Result<Self, FatError> {
        // 64-bit arithmetic: the intermediate values for absurdly large
        // volumes do not fit in 32 bits, and those must be rejected, not
        // wrapped.
        let fixed = u64::from(RESERVED_SECTORS + ROOT_SECTORS);
        let Some(available) = u64::from(total_sectors).checked_sub(fixed) else {
            return Err(FatError::VolumeSize);
        };
        for sectors_per_cluster in [1u64, 2, 4, 8, 16, 32, 64] {
            // Each cluster costs its sectors plus two bytes in each table
            // copy; start from an upper bound and correct.
            let mut clusters = available / sectors_per_cluster;
            let mut fat_sectors = 0;
            for _ in 0..3 {
                fat_sectors = ((clusters + 2) * 2).div_ceil(SECTOR_SIZE as u64);
                let Some(data) = available.checked_sub(u64::from(FAT_COPIES) * fat_sectors) else {
                    return Err(FatError::VolumeSize);
                };
                clusters = data / sectors_per_cluster;
            }
            if clusters < u64::from(MIN_CLUSTERS) {
                break;
            }
            if clusters <= u64::from(MAX_CLUSTERS) {
                return Ok(Self {
                    total_sectors,
                    sectors_per_cluster: sectors_per_cluster as u32,
                    fat_sectors: fat_sectors as u32,
                    clusters: clusters as u32,
                });
            }
        }
        Err(FatError::VolumeSize)
    }

    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR_SIZE
    }

    fn root_start(&self) -> u32 {
        RESERVED_SECTORS + FAT_COPIES * self.fat_sectors
    }

    fn data_start(&self) -> u32 {
        self.root_start() + ROOT_SECTORS
    }

    fn sector_of(&self, cluster: u32) -> u32 {
        self.data_start() + (cluster - 2) * self.sectors_per_cluster
    }
}

/// Converts one path component to the 11-byte directory form, or reports
/// that it is not a plain 8.3 name.
fn short_name(component: &str) -> Result<[u8; 11], FatError> {
    let (base, extension) = component.rsplit_once('.').unwrap_or((component, ""));
    let allowed = |byte: u8| {
        byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"_-~!#$%&'()@^`{}".contains(&byte)
    };
    if base.is_empty()
        || base.len() > 8
        || extension.len() > 3
        || (component.contains('.') && extension.is_empty())
        || !base.bytes().chain(extension.bytes()).all(allowed)
    {
        return Err(FatError::BadName);
    }
    let mut name = [b' '; 11];
    name[..base.len()].copy_from_slice(base.as_bytes());
    name[8..8 + extension.len()].copy_from_slice(extension.as_bytes());
    Ok(name)
}

enum Content<'a> {
    File(&'a [u8]),
    /// Index into the list of directories.
    Directory(usize),
}

struct Entry<'a> {
    name: [u8; 11],
    content: Content<'a>,
}

#[derive(Default)]
struct Directory<'a> {
    entries: Vec<Entry<'a>>,
    /// Index of the parent in the list of directories; the root is its
    /// own parent.
    parent: usize,
    first_cluster: u32,
}

fn directory_entry(name: &[u8; 11], attributes: u8, cluster: u32, size: u32) -> [u8; ENTRY_SIZE] {
    let mut entry = [0u8; ENTRY_SIZE];
    entry[..11].copy_from_slice(name);
    entry[11] = attributes;
    // Creation, access and modification dates; the times stay zero.
    entry[16..18].copy_from_slice(&DATE.to_le_bytes());
    entry[18..20].copy_from_slice(&DATE.to_le_bytes());
    entry[24..26].copy_from_slice(&DATE.to_le_bytes());
    entry[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    entry[28..32].copy_from_slice(&size.to_le_bytes());
    entry
}

/// Creates a FAT16 filesystem on a volume of `total_sectors` sectors
/// containing `files`.
///
/// `hidden_sectors` is the volume's first sector on its disk (recorded in
/// the boot sector, as the specification asks), `volume_id` and `label`
/// identify the volume. `write` receives a sector number relative to the
/// start of the volume and a whole number of sectors of data, and reports
/// whether it could write them; sectors it is not called for keep their
/// old contents, which the filesystem does not refer to.
pub fn format(
    total_sectors: u32,
    hidden_sectors: u32,
    volume_id: u32,
    label: &[u8; 11],
    files: &[File<'_>],
    mut write: impl FnMut(u32, &[u8]) -> bool,
) -> Result<(), FatError> {
    let geometry = Geometry::new(total_sectors)?;
    let cluster_bytes = geometry.cluster_bytes();

    // Build the directory tree. Directory 0 is the root.
    let mut directories: Vec<Directory<'_>> = vec![Directory::default()];
    for file in files {
        let mut current = 0;
        let mut components = file.path.split('/').peekable();
        while let Some(component) = components.next() {
            let name = short_name(component)?;
            let existing = directories[current]
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| match entry.content {
                    Content::Directory(index) => Some(index),
                    Content::File(_) => None,
                });
            if components.peek().is_none() {
                if existing.is_some() {
                    return Err(FatError::Conflict);
                }
                directories[current].entries.push(Entry {
                    name,
                    content: Content::File(file.data),
                });
            } else {
                current = match existing {
                    Some(Some(index)) => index,
                    Some(None) => return Err(FatError::Conflict),
                    None => {
                        let index = directories.len();
                        directories.push(Directory {
                            parent: current,
                            ..Directory::default()
                        });
                        directories[current].entries.push(Entry {
                            name,
                            content: Content::Directory(index),
                        });
                        index
                    }
                };
            }
        }
    }
    // The volume label takes one root entry.
    if directories[0].entries.len() + 1 > ROOT_ENTRIES as usize {
        return Err(FatError::RootDirectoryFull);
    }

    // Give every directory and file consecutive clusters, and record the
    // chains in the allocation table.
    let mut table = vec![0u16; geometry.clusters as usize + 2];
    table[0] = 0xFF00 | u16::from(MEDIA_FIXED_DISK);
    table[1] = END_OF_CHAIN;
    let mut next_cluster = 2u32;
    let mut allocate = |bytes: usize| -> Result<u32, FatError> {
        let count = bytes.div_ceil(cluster_bytes) as u32;
        if count == 0 {
            // An empty file has no clusters.
            return Ok(0);
        }
        let first = next_cluster;
        let end = first.checked_add(count).ok_or(FatError::NoSpace)?;
        if end > geometry.clusters + 2 {
            return Err(FatError::NoSpace);
        }
        for cluster in first..end {
            table[cluster as usize] = if cluster + 1 == end {
                END_OF_CHAIN
            } else {
                (cluster + 1) as u16
            };
        }
        next_cluster = end;
        Ok(first)
    };
    for directory in directories.iter_mut().skip(1) {
        // "." and ".." come before the entries.
        let bytes = (directory.entries.len() + 2) * ENTRY_SIZE;
        directory.first_cluster = allocate(bytes)?;
    }
    let mut file_clusters = Vec::new();
    for directory in &directories {
        for entry in &directory.entries {
            if let Content::File(data) = entry.content {
                if u32::try_from(data.len()).is_err() {
                    return Err(FatError::NoSpace);
                }
                file_clusters.push(allocate(data.len())?);
            }
        }
    }

    let mut emit = |sector: u32, data: &[u8]| -> Result<(), FatError> {
        debug_assert!(data.len().is_multiple_of(SECTOR_SIZE));
        if write(sector, data) {
            Ok(())
        } else {
            Err(FatError::Io)
        }
    };

    // Boot sector with the BIOS parameter block.
    let mut boot = [0u8; SECTOR_SIZE];
    boot[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    boot[3..11].copy_from_slice(b"OMIT OS ");
    boot[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
    boot[13] = geometry.sectors_per_cluster as u8;
    boot[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    boot[16] = FAT_COPIES as u8;
    boot[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
    if let Ok(small) = u16::try_from(total_sectors) {
        boot[19..21].copy_from_slice(&small.to_le_bytes());
    } else {
        boot[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    }
    boot[21] = MEDIA_FIXED_DISK;
    boot[22..24].copy_from_slice(&(geometry.fat_sectors as u16).to_le_bytes());
    boot[24..26].copy_from_slice(&63u16.to_le_bytes()); // sectors per track
    boot[26..28].copy_from_slice(&255u16.to_le_bytes()); // heads
    boot[28..32].copy_from_slice(&hidden_sectors.to_le_bytes());
    boot[36] = 0x80; // drive number
    boot[38] = 0x29; // extended boot signature: the next three fields exist
    boot[39..43].copy_from_slice(&volume_id.to_le_bytes());
    boot[43..54].copy_from_slice(label);
    boot[54..62].copy_from_slice(b"FAT16   ");
    boot[510..512].copy_from_slice(&[0x55, 0xAA]);
    emit(0, &boot)?;

    // Both copies of the allocation table.
    let mut table_bytes = vec![0u8; geometry.fat_sectors as usize * SECTOR_SIZE];
    for (slot, value) in table_bytes.chunks_exact_mut(2).zip(&table) {
        slot.copy_from_slice(&value.to_le_bytes());
    }
    for copy in 0..FAT_COPIES {
        emit(RESERVED_SECTORS + copy * geometry.fat_sectors, &table_bytes)?;
    }

    // Directories. Files take their clusters in the same order in which
    // they were allocated above.
    let mut file_clusters = file_clusters.into_iter();
    let mut file_data: Vec<(u32, &[u8])> = Vec::new();
    for (index, directory) in directories.iter().enumerate() {
        let mut bytes = Vec::new();
        if index == 0 {
            bytes.extend(directory_entry(label, ATTR_VOLUME_LABEL, 0, 0));
        } else {
            let parent = directories[directory.parent].first_cluster;
            bytes.extend(directory_entry(
                b".          ",
                ATTR_DIRECTORY,
                directory.first_cluster,
                0,
            ));
            bytes.extend(directory_entry(b"..         ", ATTR_DIRECTORY, parent, 0));
        }
        for entry in &directory.entries {
            match entry.content {
                Content::Directory(child) => bytes.extend(directory_entry(
                    &entry.name,
                    ATTR_DIRECTORY,
                    directories[child].first_cluster,
                    0,
                )),
                Content::File(data) => {
                    let cluster = file_clusters.next().unwrap_or(0);
                    bytes.extend(directory_entry(
                        &entry.name,
                        ATTR_ARCHIVE,
                        cluster,
                        data.len() as u32,
                    ));
                    if cluster != 0 {
                        file_data.push((cluster, data));
                    }
                }
            }
        }
        if index == 0 {
            bytes.resize(ROOT_SECTORS as usize * SECTOR_SIZE, 0);
            emit(geometry.root_start(), &bytes)?;
        } else {
            bytes.resize(bytes.len().next_multiple_of(cluster_bytes), 0);
            emit(geometry.sector_of(directory.first_cluster), &bytes)?;
        }
    }

    // File contents, a bounded piece at a time; the last sector of each
    // file is padded with zeros.
    const PIECE: usize = 64 * SECTOR_SIZE;
    for (cluster, data) in file_data {
        let mut sector = geometry.sector_of(cluster);
        let mut pieces = data.chunks_exact(PIECE);
        for piece in &mut pieces {
            emit(sector, piece)?;
            sector += (PIECE / SECTOR_SIZE) as u32;
        }
        let rest = pieces.remainder();
        if !rest.is_empty() {
            let mut padded = rest.to_vec();
            padded.resize(rest.len().next_multiple_of(SECTOR_SIZE), 0);
            emit(sector, &padded)?;
        }
    }
    Ok(())
}
