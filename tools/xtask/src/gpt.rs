// SPDX-License-Identifier: GPL-2.0-only
//! Minimal GUID Partition Table writer.
//!
//! Produces a disk layout with a protective MBR, primary and backup GPT, and
//! a single EFI System Partition. All identifiers are fixed so that the same
//! inputs always produce a byte-identical image.

/// Bytes per logical sector.
pub const SECTOR_SIZE: u64 = 512;
/// First sector of the EFI System Partition (1 MiB alignment).
pub const ESP_FIRST_LBA: u64 = 2048;

const ENTRY_COUNT: u32 = 128;
const ENTRY_SIZE: u32 = 128;
/// Sectors occupied by one copy of the partition entry array.
const ENTRY_ARRAY_SECTORS: u64 = (ENTRY_COUNT as u64 * ENTRY_SIZE as u64) / SECTOR_SIZE;
const HEADER_SIZE: u32 = 92;

/// EFI System Partition type GUID C12A7328-F81F-11D2-BA4B-00A0C93EC93B, in
/// GPT's mixed-endian on-disk encoding.
const ESP_TYPE_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
/// Fixed (not random) identifiers: "OMIT-OS-DISK-001" / "OMIT-OS-ESP--001".
const DISK_GUID: [u8; 16] = *b"OMIT-OS-DISK-001";
const ESP_GUID: [u8; 16] = *b"OMIT-OS-ESP--001";

/// CRC-32 (IEEE 802.3, reflected, as used by GPT).
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

/// Location of the EFI System Partition within a disk image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EspExtent {
    /// Byte offset of the partition.
    pub offset: u64,
    /// Size of the partition in bytes.
    pub size: u64,
}

/// Computes where the ESP lives in a disk of `disk_size` bytes. The
/// partition starts at 1 MiB and ends 1 MiB before the end of the disk.
pub fn esp_extent(disk_size: u64) -> EspExtent {
    let total = disk_size / SECTOR_SIZE;
    let last_lba = total - ESP_FIRST_LBA - 1;
    EspExtent {
        offset: ESP_FIRST_LBA * SECTOR_SIZE,
        size: (last_lba - ESP_FIRST_LBA + 1) * SECTOR_SIZE,
    }
}

fn header(total: u64, current: u64, backup: u64, entries_lba: u64, entries_crc: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(SECTOR_SIZE as usize);
    h.extend_from_slice(b"EFI PART");
    h.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // revision 1.0
    h.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // header CRC, patched below
    h.extend_from_slice(&0u32.to_le_bytes()); // reserved
    h.extend_from_slice(&current.to_le_bytes());
    h.extend_from_slice(&backup.to_le_bytes());
    h.extend_from_slice(&(2 + ENTRY_ARRAY_SECTORS).to_le_bytes()); // first usable
    h.extend_from_slice(&(total - 2 - ENTRY_ARRAY_SECTORS).to_le_bytes()); // last usable
    h.extend_from_slice(&DISK_GUID);
    h.extend_from_slice(&entries_lba.to_le_bytes());
    h.extend_from_slice(&ENTRY_COUNT.to_le_bytes());
    h.extend_from_slice(&ENTRY_SIZE.to_le_bytes());
    h.extend_from_slice(&entries_crc.to_le_bytes());
    assert_eq!(h.len(), HEADER_SIZE as usize);
    let crc = crc32(&h);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h.resize(SECTOR_SIZE as usize, 0);
    h
}

/// Writes the protective MBR and both GPT copies into `disk`, describing a
/// single EFI System Partition at [`esp_extent`]. The partition contents are
/// left untouched.
///
/// # Panics
///
/// Panics if `disk` is not a whole number of sectors or is too small to hold
/// the partition table and a non-empty partition.
pub fn write_partition_table(disk: &mut [u8]) {
    let size = disk.len() as u64;
    assert_eq!(size % SECTOR_SIZE, 0, "disk size must be sector-aligned");
    let total = size / SECTOR_SIZE;
    assert!(total > 2 * ESP_FIRST_LBA + 1, "disk image too small");
    let esp = esp_extent(size);
    let esp_last_lba = (esp.offset + esp.size) / SECTOR_SIZE - 1;
    let sector = |lba: u64| (lba * SECTOR_SIZE) as usize;

    // Protective MBR: one partition of type 0xEE covering the whole disk.
    let mbr = &mut disk[..SECTOR_SIZE as usize];
    mbr.fill(0);
    let part = &mut mbr[446..462];
    part[1..4].copy_from_slice(&[0x00, 0x02, 0x00]); // CHS of LBA 1
    part[4] = 0xEE;
    part[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    part[8..12].copy_from_slice(&1u32.to_le_bytes());
    let mbr_sectors = u32::try_from(total - 1).unwrap_or(u32::MAX);
    part[12..16].copy_from_slice(&mbr_sectors.to_le_bytes());
    mbr[510..512].copy_from_slice(&[0x55, 0xAA]);

    // Partition entry array (identical in both copies).
    let mut entries = vec![0u8; (ENTRY_COUNT * ENTRY_SIZE) as usize];
    entries[0..16].copy_from_slice(&ESP_TYPE_GUID);
    entries[16..32].copy_from_slice(&ESP_GUID);
    entries[32..40].copy_from_slice(&ESP_FIRST_LBA.to_le_bytes());
    entries[40..48].copy_from_slice(&esp_last_lba.to_le_bytes());
    // Attributes (48..56) stay zero. Name: UTF-16LE.
    for (i, unit) in "EFI System".encode_utf16().enumerate() {
        entries[56 + 2 * i..58 + 2 * i].copy_from_slice(&unit.to_le_bytes());
    }
    let entries_crc = crc32(&entries);

    let backup_lba = total - 1;
    let backup_entries_lba = backup_lba - ENTRY_ARRAY_SECTORS;

    disk[sector(1)..sector(2)].copy_from_slice(&header(total, 1, backup_lba, 2, entries_crc));
    disk[sector(2)..sector(2) + entries.len()].copy_from_slice(&entries);
    disk[sector(backup_entries_lba)..sector(backup_lba)].copy_from_slice(&entries);
    disk[sector(backup_lba)..].copy_from_slice(&header(
        total,
        backup_lba,
        1,
        backup_entries_lba,
        entries_crc,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }

    fn u64_at(b: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }

    #[test]
    fn crc32_matches_reference_vector() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn partition_table_is_self_consistent() {
        const SIZE: usize = 8 << 20;
        let mut disk = vec![0u8; SIZE];
        write_partition_table(&mut disk);
        let total = (SIZE as u64) / SECTOR_SIZE;

        // Protective MBR.
        assert_eq!(&disk[510..512], &[0x55, 0xAA]);
        assert_eq!(disk[446 + 4], 0xEE);
        assert_eq!(u32_at(&disk, 446 + 12) as u64, total - 1);

        for (lba, other, entries_lba) in [(1, total - 1, 2), (total - 1, 1, total - 33)] {
            let h = &disk[(lba * SECTOR_SIZE) as usize..][..SECTOR_SIZE as usize];
            assert_eq!(&h[..8], b"EFI PART");
            assert_eq!(u64_at(h, 24), lba);
            assert_eq!(u64_at(h, 32), other);
            assert_eq!(u64_at(h, 40), 34);
            assert_eq!(u64_at(h, 48), total - 34);
            assert_eq!(u64_at(h, 72), entries_lba);

            // Header CRC is computed with its own field zeroed.
            let mut copy = h[..HEADER_SIZE as usize].to_vec();
            copy[16..20].fill(0);
            assert_eq!(crc32(&copy), u32_at(h, 16));

            // Entry array CRC and contents.
            let start = (entries_lba * SECTOR_SIZE) as usize;
            let entries = &disk[start..start + (ENTRY_COUNT * ENTRY_SIZE) as usize];
            assert_eq!(crc32(entries), u32_at(h, 88));
            assert_eq!(&entries[..16], &ESP_TYPE_GUID);
            assert_eq!(u64_at(entries, 32), ESP_FIRST_LBA);
            assert_eq!(u64_at(entries, 40), total - ESP_FIRST_LBA - 1);
            assert!(entries[ENTRY_SIZE as usize..].iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn esp_extent_is_inside_usable_area() {
        let size = 48 << 20;
        let esp = esp_extent(size);
        assert_eq!(esp.offset, 1 << 20);
        assert_eq!(esp.size, (46 << 20) as u64);
        assert!(esp.offset + esp.size <= size - 34 * SECTOR_SIZE);
    }

    #[test]
    fn output_is_deterministic() {
        let mut a = vec![0u8; 4 << 20];
        let mut b = vec![0u8; 4 << 20];
        write_partition_table(&mut a);
        write_partition_table(&mut b);
        assert_eq!(a, b);
    }
}
