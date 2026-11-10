// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! The filesystems this crate writes are read back with the `fatfs`
//! crate, an independent implementation.

use std::io::{Cursor, Read};
use std::string::String;
use std::vec;
use std::vec::Vec;

use super::*;

/// Formats a volume of `sectors` in memory. Unwritten sectors hold 0xEE,
/// so that anything the filesystem refers to but this crate did not write
/// shows up as garbage.
fn build(sectors: u32, files: &[File<'_>]) -> Result<Vec<u8>, FatError> {
    let mut image = vec![0xEEu8; sectors as usize * SECTOR_SIZE];
    format(
        sectors,
        2048,
        0x1234_5678,
        b"OMIT OS    ",
        files,
        |sector, data| {
            let start = sector as usize * SECTOR_SIZE;
            match image.get_mut(start..start + data.len()) {
                Some(target) => {
                    target.copy_from_slice(data);
                    true
                }
                None => false,
            }
        },
    )?;
    Ok(image)
}

fn read_back(image: &mut [u8], path: &str) -> Vec<u8> {
    let fs = fatfs::FileSystem::new(Cursor::new(image), fatfs::FsOptions::new()).unwrap();
    let mut data = Vec::new();
    fs.root_dir()
        .open_file(path)
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    data
}

fn list(image: &mut [u8], path: &str) -> Vec<String> {
    let fs = fatfs::FileSystem::new(Cursor::new(image), fatfs::FsOptions::new()).unwrap();
    let root = fs.root_dir();
    let directory = if path.is_empty() {
        root
    } else {
        root.open_dir(path).unwrap()
    };
    let mut names: Vec<String> = directory
        .iter()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != "." && name != "..")
        .collect();
    names.sort();
    names
}

/// 32 MiB, the size of the installer's system partition.
const SECTORS: u32 = 65536;

#[test]
fn an_efi_system_partition_reads_back_intact() {
    let loader: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
    let kernel: Vec<u8> = (0..3_000_000u32).map(|i| (i % 241) as u8).collect();
    let initrd: Vec<u8> = (0..1_234_567u32).map(|i| (i % 239) as u8).collect();
    let files = [
        File {
            path: "EFI/BOOT/BOOTX64.EFI",
            data: &loader,
        },
        File {
            path: "EFI/OMIT/KERNEL.ELF",
            data: &kernel,
        },
        File {
            path: "EFI/OMIT/INITRD",
            data: &initrd,
        },
        File {
            path: "EFI/OMIT/CMDLINE",
            data: b"qemu-exit",
        },
    ];
    let mut image = build(SECTORS, &files).unwrap();

    {
        let fs =
            fatfs::FileSystem::new(Cursor::new(&mut image[..]), fatfs::FsOptions::new()).unwrap();
        assert_eq!(fs.fat_type(), fatfs::FatType::Fat16);
        assert_eq!(fs.volume_label(), "OMIT OS");
        assert_eq!(fs.volume_id(), 0x1234_5678);
        // Free space as the other implementation computes it from the
        // allocation table matches what the files must have used.
        let stats = fs.stats().unwrap();
        let cluster = u64::from(stats.cluster_size());
        let used: u64 = [loader.len(), kernel.len(), initrd.len(), 9]
            .iter()
            .map(|&len| (len as u64).div_ceil(cluster))
            .sum::<u64>()
            + 3; // the three directories
        assert_eq!(
            u64::from(stats.total_clusters() - stats.free_clusters()),
            used
        );
    }
    assert_eq!(list(&mut image, ""), ["EFI"]);
    assert_eq!(list(&mut image, "EFI"), ["BOOT", "OMIT"]);
    assert_eq!(
        list(&mut image, "EFI/OMIT"),
        ["CMDLINE", "INITRD", "KERNEL.ELF"]
    );
    assert_eq!(read_back(&mut image, "EFI/BOOT/BOOTX64.EFI"), loader);
    assert_eq!(read_back(&mut image, "EFI/OMIT/KERNEL.ELF"), kernel);
    assert_eq!(read_back(&mut image, "EFI/OMIT/INITRD"), initrd);
    assert_eq!(read_back(&mut image, "EFI/OMIT/CMDLINE"), b"qemu-exit");
}

#[test]
fn edge_cases_of_file_sizes_and_directory_sizes() {
    let geometry = Geometry::new(SECTORS).unwrap();
    let cluster = geometry.cluster_bytes();
    let exact = vec![0x5Au8; 2 * cluster];
    let one_more = vec![0xA5u8; cluster + 1];
    // Enough files in one directory to need several clusters for it.
    let names: Vec<String> = (0..200).map(|i| std::format!("DIR/F{i}.TXT")).collect();
    let contents: Vec<Vec<u8>> = (0..200u32).map(|i| i.to_le_bytes().to_vec()).collect();
    let mut files = vec![
        File {
            path: "EMPTY",
            data: b"",
        },
        File {
            path: "EXACT.BIN",
            data: &exact,
        },
        File {
            path: "ONEMORE.BIN",
            data: &one_more,
        },
        File {
            path: "A/B/C/DEEP.TXT",
            data: b"deep",
        },
    ];
    files.extend(
        names
            .iter()
            .zip(&contents)
            .map(|(path, data)| File { path, data }),
    );
    let mut image = build(SECTORS, &files).unwrap();

    assert_eq!(read_back(&mut image, "EMPTY"), b"");
    assert_eq!(read_back(&mut image, "EXACT.BIN"), exact);
    assert_eq!(read_back(&mut image, "ONEMORE.BIN"), one_more);
    assert_eq!(read_back(&mut image, "A/B/C/DEEP.TXT"), b"deep");
    assert_eq!(list(&mut image, "DIR").len(), 200);
    for (path, data) in names.iter().zip(&contents) {
        assert_eq!(&read_back(&mut image, path), data, "{path}");
    }
    assert_eq!(
        list(&mut image, ""),
        ["A", "DIR", "EMPTY", "EXACT.BIN", "ONEMORE.BIN"]
    );
}

#[test]
fn works_across_the_fat16_size_range() {
    // From the smallest volume FAT16 can describe to a large one; the
    // cluster size has to adapt.
    for megabytes in [3u32, 8, 16, 100, 500, 2000] {
        let sectors = megabytes * 2048;
        let geometry = Geometry::new(sectors).unwrap();
        assert!(
            (MIN_CLUSTERS..=MAX_CLUSTERS).contains(&geometry.clusters),
            "{megabytes} MiB: {geometry:?}"
        );
        // The data area must end inside the volume.
        let end = geometry.sector_of(geometry.clusters + 2);
        assert!(end <= sectors && sectors - end < geometry.sectors_per_cluster + 2);
        // Metadata only, so that even the largest case is quick: the
        // image is cut off after the first megabytes.
        if megabytes <= 16 {
            let mut image = build(
                sectors,
                &[File {
                    path: "X.TXT",
                    data: b"x",
                }],
            )
            .unwrap();
            assert_eq!(read_back(&mut image, "X.TXT"), b"x");
        }
    }
    assert_eq!(Geometry::new(100), Err(FatError::VolumeSize));
    assert_eq!(Geometry::new(2048), Err(FatError::VolumeSize));
    assert_eq!(Geometry::new(u32::MAX), Err(FatError::VolumeSize));
    assert_eq!(Geometry::new(0), Err(FatError::VolumeSize));
}

#[test]
fn rejects_what_it_cannot_represent() {
    let file = |path| [File { path, data: b"x" }];
    for bad in [
        "lowercase.txt",
        "TOOLONGNAME.TXT",
        "NAME.LONG",
        "SPA CE.TXT",
        "TRAILING.",
        "",
        "A//B",
        "DIR/",
        "UNICODÉ",
        "A.B.C",
    ] {
        assert_eq!(
            build(SECTORS, &file(bad)).err(),
            Some(FatError::BadName),
            "{bad:?}"
        );
    }
    // The same name twice, and a file used as a directory.
    let twice = [
        File {
            path: "A.TXT",
            data: b"1",
        },
        File {
            path: "A.TXT",
            data: b"2",
        },
    ];
    assert_eq!(build(SECTORS, &twice).err(), Some(FatError::Conflict));
    let through = [
        File {
            path: "A",
            data: b"1",
        },
        File {
            path: "A/B",
            data: b"2",
        },
    ];
    assert_eq!(build(SECTORS, &through).err(), Some(FatError::Conflict));

    // More data than the volume holds.
    let big = vec![0u8; 40 << 20];
    assert_eq!(
        build(
            SECTORS,
            &[File {
                path: "BIG",
                data: &big
            }]
        )
        .err(),
        Some(FatError::NoSpace)
    );
    // Too many root entries.
    let names: Vec<String> = (0..600).map(|i| std::format!("F{i}")).collect();
    let many: Vec<File<'_>> = names.iter().map(|path| File { path, data: b"" }).collect();
    assert_eq!(
        build(SECTORS, &many).err(),
        Some(FatError::RootDirectoryFull)
    );
    // A failing device is reported.
    let failed = format(
        SECTORS,
        0,
        1,
        b"LABEL      ",
        &file("A.TXT"),
        |sector, _| sector < 10,
    );
    assert_eq!(failed, Err(FatError::Io));
}

#[test]
fn output_is_deterministic_and_stays_inside_the_volume() {
    let files = [File {
        path: "EFI/BOOT/BOOTX64.EFI",
        data: &[7u8; 5000],
    }];
    let a = build(SECTORS, &files).unwrap();
    let b = build(SECTORS, &files).unwrap();
    assert!(a == b);
    // `build` fails on any write outside the image, so success above
    // already shows that; and the last sectors were never touched.
    assert!(a[a.len() - SECTOR_SIZE..].iter().all(|&byte| byte == 0xEE));
}
