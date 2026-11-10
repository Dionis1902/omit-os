// SPDX-License-Identifier: GPL-2.0-only
//! Partition layout of the boot disk image: a GUID partition table with a
//! single EFI System Partition. The table itself is built by the
//! `omit-gpt` library, which the kernel's installer uses too. All
//! identifiers are fixed so that the same inputs always produce a
//! byte-identical image.

use omit_gpt::{Partition, TYPE_EFI_SYSTEM};

/// Bytes per logical sector.
pub const SECTOR_SIZE: u64 = omit_gpt::SECTOR_SIZE;
/// First sector of the EFI System Partition (1 MiB alignment).
pub const ESP_FIRST_LBA: u64 = 2048;

/// Fixed (not random) identifiers: "OMIT-OS-DISK-001" / "OMIT-OS-ESP--001".
const DISK_GUID: [u8; 16] = *b"OMIT-OS-DISK-001";
const ESP_GUID: [u8; 16] = *b"OMIT-OS-ESP--001";

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
    let table = omit_gpt::build(
        total,
        DISK_GUID,
        &[Partition {
            type_guid: TYPE_EFI_SYSTEM,
            unique_guid: ESP_GUID,
            first_lba: ESP_FIRST_LBA,
            last_lba: (esp.offset + esp.size) / SECTOR_SIZE - 1,
            name: "EFI System".into(),
        }],
    )
    .expect("the fixed layout is valid for any disk large enough");
    disk[..table.front.len()].copy_from_slice(&table.front);
    let back = disk.len() - table.back.len();
    disk[back..].copy_from_slice(&table.back);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_layout_reads_back_as_one_efi_system_partition() {
        let mut disk = vec![0u8; 8 << 20];
        write_partition_table(&mut disk);
        let total = disk.len() as u64 / SECTOR_SIZE;
        let partitions = omit_gpt::parse(total, |lba, buffer| {
            let start = (lba * SECTOR_SIZE) as usize;
            buffer.copy_from_slice(&disk[start..start + buffer.len()]);
            true
        })
        .unwrap();
        let esp = esp_extent(disk.len() as u64);
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0].type_guid, TYPE_EFI_SYSTEM);
        assert_eq!(partitions[0].first_lba * SECTOR_SIZE, esp.offset);
        assert_eq!(partitions[0].sectors() * SECTOR_SIZE, esp.size);
        assert_eq!(partitions[0].name, "EFI System");
    }

    #[test]
    fn esp_extent_is_inside_usable_area() {
        let size = 64u64 << 20;
        let esp = esp_extent(size);
        assert_eq!(esp.offset, 1 << 20);
        assert!(esp.offset + esp.size <= size - (1 << 20) + SECTOR_SIZE);
        assert_eq!(esp.size % SECTOR_SIZE, 0);
    }

    #[test]
    fn output_is_deterministic() {
        let mut a = vec![0u8; 8 << 20];
        let mut b = vec![0u8; 8 << 20];
        write_partition_table(&mut a);
        write_partition_table(&mut b);
        assert_eq!(a, b);
    }
}
