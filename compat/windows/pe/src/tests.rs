// SPDX-License-Identifier: GPL-2.0-only
//! Tests against images assembled here byte by byte, so that each field
//! can be corrupted individually.

use std::vec;
use std::vec::Vec;

use super::*;

const PE_OFFSET: usize = 0x80;
const COFF: usize = PE_OFFSET + 4;
const OPTIONAL: usize = COFF + 20;
const OPTIONAL_SIZE: usize = 112 + 16 * 8;
const SECTION_TABLE: usize = OPTIONAL + OPTIONAL_SIZE;
const IMAGE_BASE: u64 = 0x1_4000_0000;

fn put16(data: &mut [u8], at: usize, value: u16) {
    data[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(data: &mut [u8], at: usize, value: u64) {
    data[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// File offset of the section at `rva` in the sample: sections are stored
/// back to back, 0x200 bytes each, after 0x400 bytes of headers.
fn file(rva: u32) -> usize {
    // `.data` spans two pages in memory, so `.reloc` is at 0x5000.
    let section = match rva / 0x1000 {
        5 => 3,
        page => (page - 1) as usize,
    };
    0x400 + section * 0x200 + (rva % 0x1000) as usize
}

/// A program with code, two imported libraries, one pointer that needs
/// relocating, and uninitialised data.
fn sample() -> Vec<u8> {
    let mut data = vec![0u8; 0x400 + 4 * 0x200];
    data[..2].copy_from_slice(b"MZ");
    put32(&mut data, 0x3C, PE_OFFSET as u32);
    data[PE_OFFSET..PE_OFFSET + 4].copy_from_slice(b"PE\0\0");
    put16(&mut data, COFF, MACHINE_AMD64);
    put16(&mut data, COFF + 2, 4); // sections
    put16(&mut data, COFF + 16, OPTIONAL_SIZE as u16);
    put16(&mut data, COFF + 18, 0x0022); // executable, large address aware

    put16(&mut data, OPTIONAL, 0x020B);
    put32(&mut data, OPTIONAL + 16, 0x1004); // entry point
    put64(&mut data, OPTIONAL + 24, IMAGE_BASE);
    put32(&mut data, OPTIONAL + 32, 0x1000); // section alignment
    put32(&mut data, OPTIONAL + 36, 0x200); // file alignment
    put32(&mut data, OPTIONAL + 56, 0x6000); // size of image
    put32(&mut data, OPTIONAL + 60, 0x400); // size of headers
    put16(&mut data, OPTIONAL + 68, SUBSYSTEM_CONSOLE);
    put64(&mut data, OPTIONAL + 72, 0x10_0000); // stack reserve
    put32(&mut data, OPTIONAL + 108, 16);
    // Import directory and base relocation directory.
    put32(&mut data, OPTIONAL + 112 + 8, 0x2000);
    put32(&mut data, OPTIONAL + 112 + 12, 60);
    put32(&mut data, OPTIONAL + 112 + 5 * 8, 0x5000);
    put32(&mut data, OPTIONAL + 112 + 5 * 8 + 4, 12);

    // name, rva, virtual size, file size, flags
    let sections: [(&[u8], u32, u32, u32, u32); 4] = [
        (b".text", 0x1000, 0x10, 0x200, 0x6000_0020),
        (b".rdata", 0x2000, 0x200, 0x200, 0x4000_0040),
        // Larger in memory than in the file: the rest is zero.
        (b".data", 0x3000, 0x1800, 0x200, 0xC000_0040),
        (b".reloc", 0x5000, 0x0C, 0x200, 0x4200_0040),
    ];
    for (index, (name, rva, virtual_size, file_size, flags)) in sections.iter().enumerate() {
        let at = SECTION_TABLE + index * 40;
        data[at..at + name.len()].copy_from_slice(name);
        put32(&mut data, at + 8, *virtual_size);
        put32(&mut data, at + 12, *rva);
        put32(&mut data, at + 16, *file_size);
        put32(&mut data, at + 20, (0x400 + index * 0x200) as u32);
        put32(&mut data, at + 36, *flags);
    }

    // .text
    data[file(0x1000)..file(0x1000) + 8].copy_from_slice(&[0x90; 8]);

    // .rdata: two import descriptors and the terminator at 0x2000, lookup
    // tables at 0x2040 and 0x2060, address tables at 0x2080 and 0x20A0,
    // names from 0x2100.
    let rdata = file(0x2000);
    put32(&mut data, rdata, 0x2040);
    put32(&mut data, rdata + 12, 0x2100);
    put32(&mut data, rdata + 16, 0x2080);
    // The second library has no lookup table: names are in the address
    // table.
    put32(&mut data, rdata + 20 + 12, 0x2110);
    put32(&mut data, rdata + 20 + 16, 0x20A0);
    data[file(0x2100)..file(0x2100) + 13].copy_from_slice(b"KERNEL32.dll\0");
    data[file(0x2110)..file(0x2110) + 11].copy_from_slice(b"USER32.dll\0");
    data[file(0x2122)..file(0x2122) + 12].copy_from_slice(b"ExitProcess\0");
    data[file(0x2132)..file(0x2132) + 10].copy_from_slice(b"WriteFile\0");
    data[file(0x2142)..file(0x2142) + 12].copy_from_slice(b"MessageBoxA\0");
    for table in [0x2040, 0x2080] {
        put64(&mut data, file(table), 0x2120);
        put64(&mut data, file(table + 8), 0x2130);
        put64(&mut data, file(table + 16), (1 << 63) | 77);
    }
    put64(&mut data, file(0x20A0), 0x2140);

    // .rdata, continued: an export table at 0x2180 with three functions
    // (ordinals 5, 6, 7), of which the first and third have names and
    // the third is forwarded to another library.
    put32(&mut data, OPTIONAL + 112, 0x2180);
    put32(&mut data, OPTIONAL + 112 + 4, 0x80);
    let exports = file(0x2180);
    put32(&mut data, exports + 12, 0x21D0); // library name
    put32(&mut data, exports + 16, 5); // ordinal base
    put32(&mut data, exports + 20, 3); // functions
    put32(&mut data, exports + 24, 2); // names
    put32(&mut data, exports + 28, 0x21A8);
    put32(&mut data, exports + 32, 0x21B8);
    put32(&mut data, exports + 36, 0x21C0);
    put32(&mut data, file(0x21A8), 0x1000);
    put32(&mut data, file(0x21A8) + 4, 0x1008);
    put32(&mut data, file(0x21A8) + 8, 0x21F0); // inside the table: forwarder
    put32(&mut data, file(0x21B8), 0x21DC);
    put32(&mut data, file(0x21B8) + 4, 0x21E0);
    put16(&mut data, file(0x21C0), 0);
    put16(&mut data, file(0x21C0) + 2, 2);
    data[file(0x21D0)..file(0x21D0) + 11].copy_from_slice(b"sample.dll\0");
    data[file(0x21DC)..file(0x21DC) + 4].copy_from_slice(b"add\0");
    data[file(0x21E0)..file(0x21E0) + 5].copy_from_slice(b"exit\0");
    data[file(0x21F0)..file(0x21F0) + 13].copy_from_slice(b"NTDLL.RtlExit");

    // .data: a pointer to the entry point.
    put64(&mut data, file(0x3008), IMAGE_BASE + 0x1004);

    // .reloc: one block for page 0x3000 with a DIR64 entry and padding.
    let reloc = file(0x5000);
    put32(&mut data, reloc, 0x3000);
    put32(&mut data, reloc + 4, 12);
    put16(&mut data, reloc + 8, (10 << 12) | 8);
    put16(&mut data, reloc + 10, 0);
    data
}

fn imports_of(pe: &Pe<'_>) -> PeResult<Vec<(std::string::String, std::string::String, u32)>> {
    let mut found = Vec::new();
    pe.imports(|import| {
        found.push((
            import.library.into(),
            std::format!("{}", import.symbol),
            import.slot_rva,
        ));
    })?;
    Ok(found)
}

fn relocations_of(pe: &Pe<'_>) -> PeResult<Vec<u32>> {
    let mut found = Vec::new();
    pe.relocations(|rva| found.push(rva))?;
    Ok(found)
}

#[test]
fn parses_headers_and_sections() {
    let data = sample();
    let pe = Pe::parse(&data).unwrap();
    assert_eq!(pe.entry_rva(), 0x1004);
    assert_eq!(pe.image_base(), IMAGE_BASE);
    assert_eq!(pe.size_of_image(), 0x6000);
    assert_eq!(pe.subsystem(), SUBSYSTEM_CONSOLE);
    assert_eq!(pe.stack_reserve(), 0x10_0000);
    assert!(!pe.is_dll());
    assert!(!pe.relocations_stripped());
    assert_eq!(pe.features(), Features::default());

    let sections = pe.sections();
    assert_eq!(sections.len(), 4);
    let summary: Vec<_> = sections
        .iter()
        .map(|s| (s.name(), s.rva, s.executable, s.writable))
        .collect();
    assert_eq!(
        summary,
        [
            (".text", 0x1000, true, false),
            (".rdata", 0x2000, false, false),
            (".data", 0x3000, false, true),
            (".reloc", 0x5000, false, false),
        ]
    );
}

#[test]
fn lists_imports_by_name_and_ordinal() {
    let data = sample();
    let pe = Pe::parse(&data).unwrap();
    let imports = imports_of(&pe).unwrap();
    let expected = [
        ("KERNEL32.dll", "ExitProcess", 0x2080),
        ("KERNEL32.dll", "WriteFile", 0x2088),
        ("KERNEL32.dll", "#77", 0x2090),
        ("USER32.dll", "MessageBoxA", 0x20A0),
    ];
    assert_eq!(imports.len(), expected.len());
    for (found, expected) in imports.iter().zip(expected) {
        assert_eq!((found.0.as_str(), found.1.as_str(), found.2), expected);
    }
}

#[test]
fn lists_exports_with_names_ordinals_and_forwarders() {
    let data = sample();
    let pe = Pe::parse(&data).unwrap();
    assert_eq!(pe.export_name(), Some("sample.dll"));
    let mut exports = Vec::new();
    pe.exports(|export| exports.push(export)).unwrap();
    assert_eq!(
        exports,
        [
            Export {
                name: Some("add"),
                ordinal: 5,
                target: ExportTarget::Rva(0x1000),
            },
            Export {
                name: None,
                ordinal: 6,
                target: ExportTarget::Rva(0x1008),
            },
            Export {
                name: Some("exit"),
                ordinal: 7,
                target: ExportTarget::Forwarder("NTDLL.RtlExit"),
            },
        ]
    );
}

#[test]
fn rejects_malformed_export_tables() {
    let broken = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut data = sample();
        change(&mut data);
        let pe = Pe::parse(&data).unwrap();
        pe.exports(|_| {}).unwrap_err()
    };
    let exports = file(0x2180);
    assert_eq!(
        broken(&|d| put32(d, OPTIONAL + 112, 0x9000)),
        PeError::BadExport
    );
    assert_eq!(
        broken(&|d| put32(d, exports + 20, 0x2_0000)),
        PeError::BadExport
    );
    assert_eq!(broken(&|d| put32(d, exports + 20, 200)), PeError::BadExport);
    assert_eq!(
        broken(&|d| put32(d, exports + 28, 0x7000)),
        PeError::BadExport
    );
    assert_eq!(
        broken(&|d| put32(d, exports + 24, 0x4000)),
        PeError::BadExport
    );
    // A function outside the image, and a name that is not text.
    assert_eq!(
        broken(&|d| put32(d, file(0x21A8), 0x6000)),
        PeError::BadExport
    );
    assert_eq!(
        broken(&|d| put32(d, file(0x21B8), 0x4800)),
        PeError::BadExport
    );
    // An unused slot is skipped, not an error.
    let mut data = sample();
    put32(&mut data, file(0x21A8) + 4, 0);
    let mut count = 0;
    Pe::parse(&data).unwrap().exports(|_| count += 1).unwrap();
    assert_eq!(count, 2);
    // No export table: no exports.
    let mut data = sample();
    put32(&mut data, OPTIONAL + 112, 0);
    let pe = Pe::parse(&data).unwrap();
    assert_eq!(pe.export_name(), None);
    pe.exports(|_| panic!("no exports expected")).unwrap();
}

#[test]
fn builds_and_relocates_the_image() {
    let data = sample();
    let pe = Pe::parse(&data).unwrap();
    let mut image = vec![0xAAu8; pe.size_of_image() as usize];
    pe.build_image(&mut image).unwrap();

    assert_eq!(&image[..2], b"MZ");
    assert_eq!(&image[0x1000..0x1008], &[0x90; 8]);
    // Only `virtual_size` bytes of a section are copied, the rest of the
    // image is zero — including the part of `.data` beyond the file data
    // and the gaps between sections.
    assert!(image[0x1010..0x2000].iter().all(|&b| b == 0));
    assert!(image[0x3200..0x5000].iter().all(|&b| b == 0));
    assert_eq!(&image[0x2100..0x210C], b"KERNEL32.dll");

    assert_eq!(relocations_of(&pe).unwrap(), [0x3008]);
    let pointer = |image: &[u8]| u64::from_le_bytes(image[0x3008..0x3010].try_into().unwrap());
    assert_eq!(pointer(&image), IMAGE_BASE + 0x1004);

    // At the preferred base nothing changes.
    relocate(&pe, &mut image, IMAGE_BASE).unwrap();
    assert_eq!(pointer(&image), IMAGE_BASE + 0x1004);
    // Higher and lower bases both work.
    let high = 0x1000_0001_0000;
    relocate(&pe, &mut image, high).unwrap();
    assert_eq!(pointer(&image), high + 0x1004);
    let mut image = vec![0u8; pe.size_of_image() as usize];
    pe.build_image(&mut image).unwrap();
    relocate(&pe, &mut image, 0x40_0000).unwrap();
    assert_eq!(pointer(&image), 0x40_1004);

    let mut wrong = vec![0u8; 0x5000];
    assert_eq!(pe.build_image(&mut wrong), Err(PeError::WrongImageSize));
    assert_eq!(relocate(&pe, &mut wrong, 0), Err(PeError::WrongImageSize));
}

#[test]
fn rejects_files_that_are_not_64_bit_windows_programs() {
    assert_eq!(Pe::parse(b"").unwrap_err(), PeError::NotMz);
    assert_eq!(Pe::parse(b"\x7FELF").unwrap_err(), PeError::NotMz);
    assert!(Pe::is_mz(b"MZ"));
    // A DOS program: MZ but nothing behind the PE pointer.
    assert_eq!(Pe::parse(b"MZ").unwrap_err(), PeError::NotPe);
    let mut dos = vec![0u8; 0x100];
    dos[..2].copy_from_slice(b"MZ");
    assert_eq!(Pe::parse(&dos).unwrap_err(), PeError::NotPe);
    put32(&mut dos, 0x3C, 0xFFFF_FFF0);
    assert_eq!(Pe::parse(&dos).unwrap_err(), PeError::NotPe);

    let mut data = sample();
    put16(&mut data, COFF, 0x014C);
    assert_eq!(Pe::parse(&data).unwrap_err(), PeError::WrongMachine(0x014C));
    let mut data = sample();
    put16(&mut data, COFF, 0xAA64);
    assert_eq!(Pe::parse(&data).unwrap_err(), PeError::WrongMachine(0xAA64));
    let mut data = sample();
    put16(&mut data, OPTIONAL, 0x010B);
    assert_eq!(Pe::parse(&data).unwrap_err(), PeError::NotPe32Plus(0x010B));
    let mut data = sample();
    put16(&mut data, COFF + 18, 0x0020);
    assert_eq!(Pe::parse(&data).unwrap_err(), PeError::NotExecutable);
}

#[test]
fn rejects_inconsistent_headers() {
    let corrupt = |at: usize, value: u32| {
        let mut data = sample();
        put32(&mut data, at, value);
        Pe::parse(&data).map(|_| ()).unwrap_err()
    };
    // Sections aligned to less than a page cannot be protected separately.
    assert!(matches!(
        corrupt(OPTIONAL + 32, 0x200),
        PeError::BadHeader(_)
    ));
    assert!(matches!(
        corrupt(OPTIONAL + 32, 0x3000),
        PeError::BadHeader(_)
    ));
    assert!(matches!(
        corrupt(OPTIONAL + 36, 0x2000),
        PeError::BadHeader(_)
    ));
    assert!(matches!(corrupt(OPTIONAL + 56, 0), PeError::BadHeader(_)));
    assert!(matches!(
        corrupt(OPTIONAL + 56, 0x6001),
        PeError::BadHeader(_)
    ));
    assert!(matches!(
        corrupt(OPTIONAL + 60, 0x10_0000),
        PeError::BadHeader(_)
    ));
    assert!(matches!(
        corrupt(OPTIONAL + 24, 0x4000_1000),
        PeError::BadHeader(_)
    ));
    // Entry point in a data section, or outside every section.
    assert!(matches!(
        corrupt(OPTIONAL + 16, 0x3000),
        PeError::BadHeader(_)
    ));
    assert!(matches!(
        corrupt(OPTIONAL + 16, 0x1010),
        PeError::BadHeader(_)
    ));
    assert!(matches!(corrupt(OPTIONAL + 16, 0), PeError::BadHeader(_)));

    let second = SECTION_TABLE + 40;
    // Overlapping the previous section, unaligned, beyond the image,
    // data outside the file, size overflow.
    assert_eq!(corrupt(second + 12, 0x1000), PeError::BadSection);
    assert_eq!(corrupt(second + 12, 0x2800), PeError::BadSection);
    assert_eq!(corrupt(second + 8, 0x5000), PeError::BadSection);
    assert_eq!(corrupt(second + 20, 0x10_0000), PeError::BadSection);
    assert_eq!(corrupt(second + 8, 0xFFFF_FFFF), PeError::BadSection);
    let mut data = sample();
    put16(&mut data, COFF + 2, 200);
    assert_eq!(Pe::parse(&data).unwrap_err(), PeError::BadSection);
}

#[test]
fn reports_features_that_need_loader_support() {
    let mut data = sample();
    put32(&mut data, OPTIONAL + 112 + 9 * 8, 0x3000);
    put32(&mut data, OPTIONAL + 112 + 9 * 8 + 4, 40);
    put32(&mut data, OPTIONAL + 112 + 14 * 8, 0x2000);
    let features = Pe::parse(&data).unwrap().features();
    assert!(features.tls && features.dotnet && !features.delay_imports);

    let mut data = sample();
    put16(&mut data, COFF + 18, 0x2022);
    assert!(Pe::parse(&data).unwrap().is_dll());
    let mut data = sample();
    put16(&mut data, COFF + 18, 0x0023);
    assert!(Pe::parse(&data).unwrap().relocations_stripped());
}

#[test]
fn rejects_malformed_import_tables() {
    let broken = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut data = sample();
        change(&mut data);
        let pe = Pe::parse(&data).unwrap();
        imports_of(&pe).unwrap_err()
    };
    // Directory outside the image.
    assert_eq!(
        broken(&|d| put32(d, OPTIONAL + 112 + 8, 0x9000)),
        PeError::BadImport
    );
    // Library name outside the image, and not text.
    assert_eq!(
        broken(&|d| put32(d, file(0x2000) + 12, 0x4800)),
        PeError::BadRva(0x4800)
    );
    assert_eq!(broken(&|d| d[file(0x2100)] = 0x01), PeError::BadImport);
    // Function name outside the image.
    assert_eq!(
        broken(&|d| put64(d, file(0x2040), 0x7000)),
        PeError::BadRva(0x7002)
    );
    assert_eq!(
        broken(&|d| put64(d, file(0x2040), 0x1_0000_2120)),
        PeError::BadImport
    );
    // Address table missing, unaligned, or outside every section.
    assert_eq!(
        broken(&|d| put32(d, file(0x2000) + 16, 0)),
        PeError::BadImport
    );
    assert_eq!(
        broken(&|d| put32(d, file(0x2000) + 16, 0x2081)),
        PeError::BadImport
    );
    assert_eq!(
        broken(&|d| put32(d, file(0x2000) + 16, 0x21F8)),
        PeError::BadImport
    );
    // No terminating descriptor: the table runs to the end of the section.
    assert_eq!(
        broken(&|d| {
            let start = file(0x2000);
            for at in (start + 40..start + 0x200).step_by(4) {
                put32(d, at, 0x2100);
            }
        }),
        PeError::BadImport
    );

    // No import directory at all is fine.
    let mut data = sample();
    put32(&mut data, OPTIONAL + 112 + 8, 0);
    assert!(imports_of(&Pe::parse(&data).unwrap()).unwrap().is_empty());
}

#[test]
fn rejects_malformed_relocation_tables() {
    let broken = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut data = sample();
        change(&mut data);
        let pe = Pe::parse(&data).unwrap();
        relocations_of(&pe).unwrap_err()
    };
    let reloc = file(0x5000);
    assert_eq!(broken(&|d| put32(d, reloc + 4, 4)), PeError::BadRelocation);
    assert_eq!(broken(&|d| put32(d, reloc + 4, 14)), PeError::BadRelocation);
    assert_eq!(
        broken(&|d| put32(d, reloc + 4, 400)),
        PeError::BadRelocation
    );
    // Target outside the image.
    assert_eq!(broken(&|d| put32(d, reloc, 0x5FF8)), PeError::BadRelocation);
    assert_eq!(
        broken(&|d| put32(d, reloc, 0xFFFF_FFFF)),
        PeError::BadRelocation
    );
    // The 32-bit relocation type has no meaning in a 64-bit image.
    assert_eq!(
        broken(&|d| put16(d, reloc + 8, (3 << 12) | 8)),
        PeError::UnsupportedRelocation(3)
    );
    assert_eq!(
        broken(&|d| put32(d, OPTIONAL + 112 + 5 * 8 + 4, 0x1000)),
        PeError::BadRelocation
    );
}

/// Runs everything the loader would run; must never panic.
fn exercise(data: &[u8]) {
    if let Ok(pe) = Pe::parse(data) {
        let _ = imports_of(&pe);
        let _ = relocations_of(&pe);
        let _ = pe.export_name();
        let _ = pe.exports(|_| {});
        let mut image = vec![0u8; pe.size_of_image() as usize];
        if pe.build_image(&mut image).is_ok() {
            let _ = relocate(&pe, &mut image, 0x1000_0000_0000);
        }
    }
}

#[test]
fn survives_truncation_at_every_length() {
    let data = sample();
    for length in 0..data.len() {
        exercise(&data[..length]);
    }
}

#[test]
fn survives_corruption() {
    // Deterministic pseudo-random corruption of the headers and tables,
    // where every byte matters. `size_of_image` is kept small enough to
    // allocate.
    let original = sample();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..200_000 {
        let mut data = original.clone();
        for _ in 0..=(next() % 4) {
            let regions = [(0, 0x400), (file(0x2000), 0x200), (file(0x5000), 0x10)];
            let (start, length) = regions[(next() % 3) as usize];
            let at = start + (next() as usize % length);
            data[at] = match next() % 3 {
                0 => next() as u8,
                1 => data[at] ^ (1 << (next() % 8)),
                _ => [0, 0xFF, 0x7F, 0x80][(next() % 4) as usize],
            };
        }
        if u32::from_le_bytes(data[OPTIONAL + 56..OPTIONAL + 60].try_into().unwrap()) > 0x100_0000 {
            continue;
        }
        exercise(&data);
    }
}
