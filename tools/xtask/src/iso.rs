// SPDX-License-Identifier: GPL-2.0-only
//! Minimal ISO 9660 + El Torito writer.
//!
//! Produces a CD/DVD image that UEFI firmware can boot: the El Torito boot
//! catalog has a single EFI platform entry pointing at an embedded FAT
//! image (the same EFI system partition contents as on the disk image).
//! There is no BIOS boot entry and no hybrid MBR/GPT, so the ISO is for
//! optical media and virtual machines; for USB sticks use the disk image.
//!
//! The ISO 9660 directory also lists the boot image as `EFI.IMG` and a
//! `README.TXT`, so the disc is not empty when inspected from another OS.
//! No timestamps are recorded: the image is reproducible.

/// ISO 9660 logical sector size.
const SECTOR: usize = 2048;

const LBA_PVD: u32 = 16;
const LBA_BOOT_RECORD: u32 = 17;
const LBA_TERMINATOR: u32 = 18;
const LBA_PATH_TABLE_LE: u32 = 19;
const LBA_PATH_TABLE_BE: u32 = 20;
const LBA_ROOT_DIR: u32 = 21;
const LBA_BOOT_CATALOG: u32 = 22;
const LBA_README: u32 = 23;
const LBA_BOOT_IMAGE: u32 = 24;

/// Path table with only the root directory: 10 bytes.
const PATH_TABLE_SIZE: u32 = 10;

fn both_u16(value: u16) -> [u8; 4] {
    let mut out = [0; 4];
    out[..2].copy_from_slice(&value.to_le_bytes());
    out[2..].copy_from_slice(&value.to_be_bytes());
    out
}

fn both_u32(value: u32) -> [u8; 8] {
    let mut out = [0; 8];
    out[..4].copy_from_slice(&value.to_le_bytes());
    out[4..].copy_from_slice(&value.to_be_bytes());
    out
}

/// Copies `text` into `field`, padding with spaces (ISO "a-characters").
fn put_padded(field: &mut [u8], text: &str) {
    field.fill(b' ');
    field[..text.len()].copy_from_slice(text.as_bytes());
}

/// Builds a directory record for `name` covering `size` bytes at `lba`.
fn directory_record(name: &[u8], lba: u32, size: u32, is_dir: bool) -> Vec<u8> {
    let length = 33 + name.len() + (1 - name.len() % 2);
    let mut record = vec![0u8; length];
    record[0] = length as u8;
    record[2..10].copy_from_slice(&both_u32(lba));
    record[10..18].copy_from_slice(&both_u32(size));
    // Bytes 18..25: recording date and time, left zero ("not specified").
    record[25] = if is_dir { 0x02 } else { 0x00 };
    record[28..32].copy_from_slice(&both_u16(1)); // volume sequence number
    record[32] = name.len() as u8;
    record[33..33 + name.len()].copy_from_slice(name);
    record
}

/// Builds a UEFI-bootable ISO image.
///
/// `boot_image` is a FAT filesystem image containing
/// `\EFI\BOOT\BOOTX64.EFI`; `readme` is placed on the disc as `README.TXT`.
///
/// # Panics
///
/// Panics if `readme` does not fit in one sector or the image would exceed
/// the 32-bit sector count of ISO 9660.
pub fn build(boot_image: &[u8], volume_label: &str, readme: &str) -> Vec<u8> {
    assert!(readme.len() <= SECTOR, "README must fit in one sector");
    let image_sectors = boot_image.len().div_ceil(SECTOR);
    let total_sectors = LBA_BOOT_IMAGE as usize + image_sectors;
    let total = u32::try_from(total_sectors).expect("ISO image too large");
    let image_size = u32::try_from(boot_image.len()).expect("boot image too large");

    let mut iso = vec![0u8; total_sectors * SECTOR];
    let sector = |lba: u32| lba as usize * SECTOR;

    // --- Primary volume descriptor --------------------------------------
    let root_record = directory_record(&[0], LBA_ROOT_DIR, SECTOR as u32, true);
    {
        let pvd = &mut iso[sector(LBA_PVD)..sector(LBA_PVD + 1)];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        put_padded(&mut pvd[8..40], "");
        put_padded(&mut pvd[40..72], volume_label);
        pvd[80..88].copy_from_slice(&both_u32(total));
        pvd[120..124].copy_from_slice(&both_u16(1)); // volume set size
        pvd[124..128].copy_from_slice(&both_u16(1)); // volume sequence number
        pvd[128..132].copy_from_slice(&both_u16(SECTOR as u16));
        pvd[132..140].copy_from_slice(&both_u32(PATH_TABLE_SIZE));
        pvd[140..144].copy_from_slice(&LBA_PATH_TABLE_LE.to_le_bytes());
        pvd[148..152].copy_from_slice(&LBA_PATH_TABLE_BE.to_be_bytes());
        pvd[156..156 + root_record.len()].copy_from_slice(&root_record);
        // Volume set, publisher, preparer, application identifiers and the
        // three file identifiers: all blank.
        put_padded(&mut pvd[190..813], "");
        // Four dates, each 16 ASCII zeros plus a zero time-zone byte.
        for date in 0..4 {
            let start = 813 + date * 17;
            pvd[start..start + 16].fill(b'0');
            pvd[start + 16] = 0;
        }
        pvd[881] = 1; // file structure version
    }

    // --- El Torito boot record ------------------------------------------
    {
        let boot = &mut iso[sector(LBA_BOOT_RECORD)..sector(LBA_BOOT_RECORD + 1)];
        boot[0] = 0;
        boot[1..6].copy_from_slice(b"CD001");
        boot[6] = 1;
        boot[7..30].copy_from_slice(b"EL TORITO SPECIFICATION");
        boot[71..75].copy_from_slice(&LBA_BOOT_CATALOG.to_le_bytes());
    }

    // --- Volume descriptor set terminator -------------------------------
    {
        let end = &mut iso[sector(LBA_TERMINATOR)..sector(LBA_TERMINATOR + 1)];
        end[0] = 255;
        end[1..6].copy_from_slice(b"CD001");
        end[6] = 1;
    }

    // --- Path tables (root directory only) ------------------------------
    for (lba, big_endian) in [(LBA_PATH_TABLE_LE, false), (LBA_PATH_TABLE_BE, true)] {
        let table = &mut iso[sector(lba)..sector(lba) + PATH_TABLE_SIZE as usize];
        table[0] = 1; // identifier length
        if big_endian {
            table[2..6].copy_from_slice(&LBA_ROOT_DIR.to_be_bytes());
            table[6..8].copy_from_slice(&1u16.to_be_bytes());
        } else {
            table[2..6].copy_from_slice(&LBA_ROOT_DIR.to_le_bytes());
            table[6..8].copy_from_slice(&1u16.to_le_bytes());
        }
    }

    // --- Root directory -------------------------------------------------
    {
        let mut records = Vec::new();
        records.extend(directory_record(&[0], LBA_ROOT_DIR, SECTOR as u32, true));
        records.extend(directory_record(&[1], LBA_ROOT_DIR, SECTOR as u32, true));
        // Entries must be sorted by name.
        records.extend(directory_record(
            b"BOOT.CAT;1",
            LBA_BOOT_CATALOG,
            SECTOR as u32,
            false,
        ));
        records.extend(directory_record(
            b"EFI.IMG;1",
            LBA_BOOT_IMAGE,
            image_size,
            false,
        ));
        records.extend(directory_record(
            b"README.TXT;1",
            LBA_README,
            readme.len() as u32,
            false,
        ));
        iso[sector(LBA_ROOT_DIR)..sector(LBA_ROOT_DIR) + records.len()].copy_from_slice(&records);
    }

    // --- Boot catalog ---------------------------------------------------
    {
        let catalog = &mut iso[sector(LBA_BOOT_CATALOG)..sector(LBA_BOOT_CATALOG + 1)];
        // Validation entry.
        catalog[0] = 1;
        catalog[1] = 0xEF; // platform: EFI
        catalog[30] = 0x55;
        catalog[31] = 0xAA;
        // The 16-bit words of the entry must sum to zero.
        let sum = catalog[..32].chunks_exact(2).fold(0u16, |sum, w| {
            sum.wrapping_add(u16::from_le_bytes([w[0], w[1]]))
        });
        catalog[28..30].copy_from_slice(&0u16.wrapping_sub(sum).to_le_bytes());

        // Initial/default entry: bootable, no emulation.
        let entry = &mut catalog[32..64];
        entry[0] = 0x88;
        // Sector count in 512-byte units; saturates for large images, which
        // UEFI firmware handles by reading the FAT filesystem's own size.
        let count = u16::try_from(boot_image.len().div_ceil(512)).unwrap_or(u16::MAX);
        entry[6..8].copy_from_slice(&count.to_le_bytes());
        entry[8..12].copy_from_slice(&LBA_BOOT_IMAGE.to_le_bytes());
    }

    // --- Files ----------------------------------------------------------
    iso[sector(LBA_README)..sector(LBA_README) + readme.len()].copy_from_slice(readme.as_bytes());
    iso[sector(LBA_BOOT_IMAGE)..sector(LBA_BOOT_IMAGE) + boot_image.len()]
        .copy_from_slice(boot_image);
    iso
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn be32(bytes: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn volume_descriptors_are_consistent() {
        let image = vec![0xA5u8; 5000];
        let iso = build(&image, "OMIT_OS", "hello\n");
        assert_eq!(iso.len() % SECTOR, 0);
        let sectors = (iso.len() / SECTOR) as u32;
        assert_eq!(sectors, LBA_BOOT_IMAGE + 3);

        let pvd = &iso[16 * SECTOR..17 * SECTOR];
        assert_eq!(&pvd[..7], b"\x01CD001\x01");
        assert_eq!(&pvd[40..47], b"OMIT_OS");
        assert_eq!(le32(pvd, 80), sectors);
        assert_eq!(be32(pvd, 84), sectors);
        assert_eq!(u16::from_le_bytes([pvd[128], pvd[129]]), 2048);
        assert_eq!(le32(pvd, 140), LBA_PATH_TABLE_LE);
        assert_eq!(be32(pvd, 148), LBA_PATH_TABLE_BE);
        assert_eq!(le32(pvd, 156 + 2), LBA_ROOT_DIR, "root directory extent");

        let boot = &iso[17 * SECTOR..18 * SECTOR];
        assert_eq!(&boot[..7], b"\x00CD001\x01");
        assert_eq!(&boot[7..30], b"EL TORITO SPECIFICATION");
        assert_eq!(le32(boot, 71), LBA_BOOT_CATALOG);
        assert_eq!(&iso[18 * SECTOR..18 * SECTOR + 7], b"\xFFCD001\x01");
    }

    #[test]
    fn boot_catalog_points_at_the_efi_image() {
        let image: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let iso = build(&image, "X", "");
        let catalog = &iso[LBA_BOOT_CATALOG as usize * SECTOR..][..64];
        assert_eq!(catalog[0], 1);
        assert_eq!(catalog[1], 0xEF);
        assert_eq!(&catalog[30..32], &[0x55, 0xAA]);
        let sum = catalog[..32].chunks_exact(2).fold(0u16, |s, w| {
            s.wrapping_add(u16::from_le_bytes([w[0], w[1]]))
        });
        assert_eq!(sum, 0, "validation entry checksum");
        assert_eq!(catalog[32], 0x88);
        assert_eq!(u16::from_le_bytes([catalog[38], catalog[39]]), 20);
        let lba = le32(catalog, 40) as usize;
        assert_eq!(&iso[lba * SECTOR..lba * SECTOR + image.len()], &image[..]);
    }

    #[test]
    fn root_directory_lists_files_in_order() {
        let iso = build(&[1, 2, 3], "X", "readme text");
        let dir = &iso[LBA_ROOT_DIR as usize * SECTOR..][..SECTOR];
        let mut names = Vec::new();
        let mut at = 0;
        while dir[at] != 0 {
            let len = dir[at] as usize;
            let name_len = dir[at + 32] as usize;
            names.push(dir[at + 33..at + 33 + name_len].to_vec());
            if names.len() == 5 {
                // README.TXT: check extent and size.
                let lba = le32(dir, at + 2) as usize;
                let size = le32(dir, at + 10) as usize;
                assert_eq!(&iso[lba * SECTOR..lba * SECTOR + size], b"readme text");
            }
            assert_eq!(len % 2, 0, "records have even length");
            at += len;
        }
        assert_eq!(
            names,
            [
                vec![0],
                vec![1],
                b"BOOT.CAT;1".to_vec(),
                b"EFI.IMG;1".to_vec(),
                b"README.TXT;1".to_vec()
            ]
        );
    }

    #[test]
    fn output_is_deterministic() {
        assert_eq!(build(&[7; 100], "L", "r"), build(&[7; 100], "L", "r"));
    }
}
