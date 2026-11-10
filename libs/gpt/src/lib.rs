// SPDX-License-Identifier: GPL-2.0-only
//! GUID Partition Tables.
//!
//! A GPT-partitioned disk has a protective MBR in sector 0, a header in
//! sector 1 followed by an array of 128 partition entries, and a backup
//! copy of both at the end of the disk. Header and entry array carry
//! CRC-32 checksums. This crate builds such a table ([`build`]) and reads
//! one back ([`parse`]); it is used by the image-building tool, by the
//! kernel to find partitions, and by the installer.
//!
//! Written from the UEFI specification. Not supported: sector sizes other
//! than 512 bytes, entry arrays that do not have 128 entries of 128 bytes
//! (what every common tool writes), and repairing a damaged primary table
//! from the backup — a table that fails its checksums is reported as
//! corrupt.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Bytes per sector.
pub const SECTOR_SIZE: u64 = 512;

const ENTRY_COUNT: u32 = 128;
const ENTRY_SIZE: u32 = 128;
/// Sectors occupied by one copy of the partition entry array.
const ENTRY_ARRAY_SECTORS: u64 = (ENTRY_COUNT as u64 * ENTRY_SIZE as u64) / SECTOR_SIZE;
const HEADER_SIZE: u32 = 92;

/// First sector a partition may use.
pub const FIRST_USABLE_LBA: u64 = 2 + ENTRY_ARRAY_SECTORS;
/// Sectors at the start of the disk covered by [`Table::front`].
pub const FRONT_SECTORS: u64 = FIRST_USABLE_LBA;
/// Sectors at the end of the disk covered by [`Table::back`].
pub const BACK_SECTORS: u64 = ENTRY_ARRAY_SECTORS + 1;

/// A GUID in GPT's on-disk encoding (the first three groups
/// little-endian).
pub type Guid = [u8; 16];

/// EFI System Partition, C12A7328-F81F-11D2-BA4B-00A0C93EC93B.
pub const TYPE_EFI_SYSTEM: Guid = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
/// Omit OS data partition, 9D3C1E5A-7B24-4F86-A1D0-4F4D49544F53: the
/// partition an installed system mounts at `/data`.
pub const TYPE_OMIT_DATA: Guid = [
    0x5A, 0x1E, 0x3C, 0x9D, 0x24, 0x7B, 0x86, 0x4F, 0xA1, 0xD0, 0x4F, 0x4D, 0x49, 0x54, 0x4F, 0x53,
];

/// One partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// What the partition is for.
    pub type_guid: Guid,
    /// Identifies this particular partition.
    pub unique_guid: Guid,
    /// First sector.
    pub first_lba: u64,
    /// Last sector (inclusive).
    pub last_lba: u64,
    /// Human-readable name, at most 36 UTF-16 units.
    pub name: String,
}

impl Partition {
    /// Number of sectors.
    pub fn sectors(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }
}

/// Why a table could not be built or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GptError {
    /// The disk is too small for a partition table.
    DiskTooSmall,
    /// A partition lies outside the usable area, overlaps another, has a
    /// name that is too long, or there are too many partitions.
    InvalidPartition,
    /// The disk has no GPT (no signature in sector 1).
    NoTable,
    /// The table is damaged or internally inconsistent.
    Corrupt(&'static str),
    /// The table uses a layout this crate does not handle.
    Unsupported(&'static str),
    /// A sector could not be read.
    Io,
}

impl core::fmt::Display for GptError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DiskTooSmall => write!(f, "disk is too small for a partition table"),
            Self::InvalidPartition => write!(f, "invalid partition layout"),
            Self::NoTable => write!(f, "no GUID partition table"),
            Self::Corrupt(what) => write!(f, "damaged partition table: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported partition table: {what}"),
            Self::Io => write!(f, "read error"),
        }
    }
}

/// CRC-32 (IEEE 802.3, reflected), as GPT uses it.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A partition table ready to be written to a disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Contents of the first [`FRONT_SECTORS`] sectors: protective MBR,
    /// primary header, entry array.
    pub front: Vec<u8>,
    /// Contents of the last [`BACK_SECTORS`] sectors: backup entry array,
    /// backup header.
    pub back: Vec<u8>,
}

fn header(
    total: u64,
    disk_guid: &Guid,
    current: u64,
    other: u64,
    entries_lba: u64,
    entries_crc: u32,
) -> Vec<u8> {
    let mut h = Vec::with_capacity(SECTOR_SIZE as usize);
    h.extend_from_slice(b"EFI PART");
    h.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // revision 1.0
    h.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // header CRC, patched below
    h.extend_from_slice(&0u32.to_le_bytes()); // reserved
    h.extend_from_slice(&current.to_le_bytes());
    h.extend_from_slice(&other.to_le_bytes());
    h.extend_from_slice(&FIRST_USABLE_LBA.to_le_bytes());
    h.extend_from_slice(&last_usable_lba(total).to_le_bytes());
    h.extend_from_slice(disk_guid);
    h.extend_from_slice(&entries_lba.to_le_bytes());
    h.extend_from_slice(&ENTRY_COUNT.to_le_bytes());
    h.extend_from_slice(&ENTRY_SIZE.to_le_bytes());
    h.extend_from_slice(&entries_crc.to_le_bytes());
    let crc = crc32(&h);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h.resize(SECTOR_SIZE as usize, 0);
    h
}

/// Last sector a partition may use on a disk of `total` sectors.
pub const fn last_usable_lba(total: u64) -> u64 {
    total - BACK_SECTORS - 1
}

/// Builds the partition table for a disk of `total_sectors` holding
/// `partitions`, which must lie in the usable area and not overlap.
pub fn build(
    total_sectors: u64,
    disk_guid: Guid,
    partitions: &[Partition],
) -> Result<Table, GptError> {
    if total_sectors < FRONT_SECTORS + BACK_SECTORS + 1 {
        return Err(GptError::DiskTooSmall);
    }
    if partitions.len() > ENTRY_COUNT as usize {
        return Err(GptError::InvalidPartition);
    }
    let last_usable = last_usable_lba(total_sectors);
    for (index, partition) in partitions.iter().enumerate() {
        let inside = partition.first_lba >= FIRST_USABLE_LBA
            && partition.first_lba <= partition.last_lba
            && partition.last_lba <= last_usable;
        let overlaps = partitions[..index].iter().any(|other| {
            partition.first_lba <= other.last_lba && other.first_lba <= partition.last_lba
        });
        if !inside
            || overlaps
            || partition.type_guid == [0; 16]
            || partition.name.encode_utf16().count() > 36
        {
            return Err(GptError::InvalidPartition);
        }
    }

    let mut entries = vec![0u8; (ENTRY_COUNT * ENTRY_SIZE) as usize];
    for (partition, entry) in partitions
        .iter()
        .zip(entries.chunks_exact_mut(ENTRY_SIZE as usize))
    {
        entry[0..16].copy_from_slice(&partition.type_guid);
        entry[16..32].copy_from_slice(&partition.unique_guid);
        entry[32..40].copy_from_slice(&partition.first_lba.to_le_bytes());
        entry[40..48].copy_from_slice(&partition.last_lba.to_le_bytes());
        // Attributes (48..56) stay zero. Name: UTF-16LE.
        for (i, unit) in partition.name.encode_utf16().enumerate() {
            entry[56 + 2 * i..58 + 2 * i].copy_from_slice(&unit.to_le_bytes());
        }
    }
    let entries_crc = crc32(&entries);

    // Protective MBR: one partition of type 0xEE covering the whole disk,
    // so that tools unaware of GPT see the disk as in use.
    let mut front = vec![0u8; SECTOR_SIZE as usize];
    let part = &mut front[446..462];
    part[1..4].copy_from_slice(&[0x00, 0x02, 0x00]); // CHS of LBA 1
    part[4] = 0xEE;
    part[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    part[8..12].copy_from_slice(&1u32.to_le_bytes());
    let mbr_sectors = u32::try_from(total_sectors - 1).unwrap_or(u32::MAX);
    part[12..16].copy_from_slice(&mbr_sectors.to_le_bytes());
    front[510..512].copy_from_slice(&[0x55, 0xAA]);

    let backup_lba = total_sectors - 1;
    let backup_entries_lba = backup_lba - ENTRY_ARRAY_SECTORS;
    front.extend(header(
        total_sectors,
        &disk_guid,
        1,
        backup_lba,
        2,
        entries_crc,
    ));
    front.extend_from_slice(&entries);
    let mut back = entries;
    back.extend(header(
        total_sectors,
        &disk_guid,
        backup_lba,
        1,
        backup_entries_lba,
        entries_crc,
    ));
    Ok(Table { front, back })
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    let mut value = [0u8; 4];
    value.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(value)
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}

/// Reads the partition table of a disk of `total_sectors`. `read` fills a
/// 512-byte buffer with the given sector and reports whether that worked.
/// Returns the partitions in table order.
pub fn parse(
    total_sectors: u64,
    mut read: impl FnMut(u64, &mut [u8]) -> bool,
) -> Result<Vec<Partition>, GptError> {
    if total_sectors < FRONT_SECTORS + BACK_SECTORS + 1 {
        return Err(GptError::NoTable);
    }
    let mut sector = [0u8; SECTOR_SIZE as usize];
    if !read(1, &mut sector) {
        return Err(GptError::Io);
    }
    if &sector[..8] != b"EFI PART" {
        return Err(GptError::NoTable);
    }
    let header_size = u32_at(&sector, 12) as usize;
    if !(HEADER_SIZE as usize..=SECTOR_SIZE as usize).contains(&header_size) {
        return Err(GptError::Corrupt("header size"));
    }
    let stored_crc = u32_at(&sector, 16);
    let mut copy = sector;
    copy[16..20].fill(0);
    if crc32(&copy[..header_size]) != stored_crc {
        return Err(GptError::Corrupt("header checksum"));
    }
    if u64_at(&sector, 24) != 1 {
        return Err(GptError::Corrupt("header position"));
    }
    let (first_usable, last_usable) = (u64_at(&sector, 40), u64_at(&sector, 48));
    let entries_lba = u64_at(&sector, 72);
    let entry_count = u32_at(&sector, 80);
    let entry_size = u32_at(&sector, 84);
    let entries_crc = u32_at(&sector, 88);
    if entry_size != ENTRY_SIZE || entry_count != ENTRY_COUNT {
        return Err(GptError::Unsupported("entry array geometry"));
    }
    if first_usable > last_usable
        || last_usable >= total_sectors
        || entries_lba < 2
        || entries_lba
            .checked_add(ENTRY_ARRAY_SECTORS)
            .is_none_or(|end| end > first_usable)
    {
        return Err(GptError::Corrupt("header fields"));
    }

    let mut entries = vec![0u8; (ENTRY_COUNT * ENTRY_SIZE) as usize];
    for (index, chunk) in entries.chunks_exact_mut(SECTOR_SIZE as usize).enumerate() {
        if !read(entries_lba + index as u64, chunk) {
            return Err(GptError::Io);
        }
    }
    if crc32(&entries) != entries_crc {
        return Err(GptError::Corrupt("entry array checksum"));
    }

    let mut partitions = Vec::new();
    for entry in entries.chunks_exact(ENTRY_SIZE as usize) {
        if entry[..16].iter().all(|&byte| byte == 0) {
            continue; // unused entry
        }
        let (first_lba, last_lba) = (u64_at(entry, 32), u64_at(entry, 40));
        if first_lba < first_usable || first_lba > last_lba || last_lba > last_usable {
            return Err(GptError::Corrupt("partition bounds"));
        }
        let units: Vec<u16> = entry[56..128]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        let mut type_guid = [0u8; 16];
        type_guid.copy_from_slice(&entry[..16]);
        let mut unique_guid = [0u8; 16];
        unique_guid.copy_from_slice(&entry[16..32]);
        partitions.push(Partition {
            type_guid,
            unique_guid,
            first_lba,
            last_lba,
            name: char::decode_utf16(units)
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect(),
        });
    }
    Ok(partitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOTAL: u64 = 65536; // 32 MiB

    fn sample() -> [Partition; 2] {
        [
            Partition {
                type_guid: TYPE_EFI_SYSTEM,
                unique_guid: *b"0123456789abcdef",
                first_lba: 2048,
                last_lba: 34815,
                name: "EFI System".into(),
            },
            Partition {
                type_guid: TYPE_OMIT_DATA,
                unique_guid: *b"fedcba9876543210",
                first_lba: 34816,
                last_lba: last_usable_lba(TOTAL),
                name: "Omit OS data".into(),
            },
        ]
    }

    /// A disk image with the table written to it.
    fn disk(table: &Table) -> Vec<u8> {
        let mut disk = vec![0u8; (TOTAL * SECTOR_SIZE) as usize];
        disk[..table.front.len()].copy_from_slice(&table.front);
        let back = disk.len() - table.back.len();
        disk[back..].copy_from_slice(&table.back);
        disk
    }

    fn read_from(disk: &[u8]) -> impl FnMut(u64, &mut [u8]) -> bool + '_ {
        |lba, buffer| {
            let start = (lba * SECTOR_SIZE) as usize;
            match disk.get(start..start + buffer.len()) {
                Some(sector) => {
                    buffer.copy_from_slice(sector);
                    true
                }
                None => false,
            }
        }
    }

    #[test]
    fn crc32_matches_reference_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn builds_a_consistent_table_and_reads_it_back() {
        let partitions = sample();
        let table = build(TOTAL, *b"disk-guid-000001", &partitions).unwrap();
        assert_eq!(table.front.len() as u64, FRONT_SECTORS * SECTOR_SIZE);
        assert_eq!(table.back.len() as u64, BACK_SECTORS * SECTOR_SIZE);
        let disk = disk(&table);

        // Protective MBR.
        assert_eq!(&disk[510..512], &[0x55, 0xAA]);
        assert_eq!(disk[446 + 4], 0xEE);
        // Primary and backup headers point at each other and at their own
        // entry arrays, and both checksums hold.
        let primary = &disk[512..1024];
        let backup = &disk[disk.len() - 512..];
        for (header, own, other, entries) in [
            (primary, 1, TOTAL - 1, 2),
            (backup, TOTAL - 1, 1, TOTAL - 1 - ENTRY_ARRAY_SECTORS),
        ] {
            assert_eq!(&header[..8], b"EFI PART");
            assert_eq!(u64_at(header, 24), own);
            assert_eq!(u64_at(header, 32), other);
            assert_eq!(u64_at(header, 72), entries);
            let mut copy = header[..92].to_vec();
            copy[16..20].fill(0);
            assert_eq!(crc32(&copy), u32_at(header, 16));
            let start = (entries * SECTOR_SIZE) as usize;
            assert_eq!(crc32(&disk[start..start + 128 * 128]), u32_at(header, 88));
        }

        assert_eq!(parse(TOTAL, read_from(&disk)).unwrap(), partitions);
        assert_eq!(partitions[0].sectors(), 32768);
    }

    #[test]
    fn output_is_deterministic() {
        let a = build(TOTAL, [7; 16], &sample()).unwrap();
        let b = build(TOTAL, [7; 16], &sample()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_invalid_layouts() {
        let good = sample();
        let build_with = |change: &dyn Fn(&mut [Partition; 2])| {
            let mut partitions = good.clone();
            change(&mut partitions);
            build(TOTAL, [1; 16], &partitions)
        };
        assert!(build_with(&|_| {}).is_ok());
        let invalid = Err(GptError::InvalidPartition);
        assert_eq!(build_with(&|p| p[0].first_lba = 10), invalid);
        assert_eq!(build_with(&|p| p[1].last_lba = TOTAL - 1), invalid);
        assert_eq!(build_with(&|p| p[1].first_lba = 30000), invalid);
        assert_eq!(build_with(&|p| p[0].last_lba = 100), invalid);
        assert_eq!(build_with(&|p| p[0].type_guid = [0; 16]), invalid);
        assert_eq!(build_with(&|p| p[0].name = "x".repeat(37)), invalid);
        assert_eq!(build(60, [1; 16], &[]), Err(GptError::DiskTooSmall));
        assert!(build(TOTAL, [1; 16], &[]).is_ok());
    }

    #[test]
    fn detects_missing_and_damaged_tables() {
        let blank = vec![0u8; (TOTAL * SECTOR_SIZE) as usize];
        assert_eq!(parse(TOTAL, read_from(&blank)), Err(GptError::NoTable));
        assert_eq!(parse(TOTAL, |_, _| false), Err(GptError::Io));
        assert_eq!(parse(10, read_from(&blank)), Err(GptError::NoTable));

        let table = build(TOTAL, [1; 16], &sample()).unwrap();
        let good = disk(&table);
        let damaged = |offset: usize| {
            let mut disk = good.clone();
            disk[offset] ^= 0x40;
            parse(TOTAL, read_from(&disk))
        };
        // A flipped bit anywhere in the header or the entries is noticed.
        assert_eq!(damaged(512 + 40), Err(GptError::Corrupt("header checksum")));
        assert_eq!(damaged(512 + 16), Err(GptError::Corrupt("header checksum")));
        assert_eq!(
            damaged(1024 + 33),
            Err(GptError::Corrupt("entry array checksum"))
        );
        assert_eq!(
            damaged(1024 + 127 * 128),
            Err(GptError::Corrupt("entry array checksum"))
        );
        assert_eq!(damaged(512), Err(GptError::NoTable));
    }

    #[test]
    fn survives_arbitrary_headers() {
        // Headers with valid checksums but hostile field values must be
        // rejected, not followed.
        let table = build(TOTAL, [1; 16], &sample()).unwrap();
        let good = disk(&table);
        let forged = |offset: usize, value: u64, width: usize| {
            let mut disk = good.clone();
            disk[512 + offset..512 + offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
            disk[512 + 16..512 + 20].fill(0);
            let crc = crc32(&disk[512..512 + 92]);
            disk[512 + 16..512 + 20].copy_from_slice(&crc.to_le_bytes());
            parse(TOTAL, read_from(&disk))
        };
        assert!(matches!(
            forged(72, u64::MAX - 5, 8),
            Err(GptError::Corrupt(_))
        ));
        assert!(matches!(forged(72, 0, 8), Err(GptError::Corrupt(_))));
        assert!(matches!(forged(48, u64::MAX, 8), Err(GptError::Corrupt(_))));
        assert!(matches!(forged(40, u64::MAX, 8), Err(GptError::Corrupt(_))));
        assert!(matches!(forged(80, 4096, 4), Err(GptError::Unsupported(_))));
        assert!(matches!(forged(84, 512, 4), Err(GptError::Unsupported(_))));
        assert!(matches!(forged(12, 4096, 4), Err(GptError::Corrupt(_))));
        assert!(matches!(forged(24, 7, 8), Err(GptError::Corrupt(_))));
    }
}
