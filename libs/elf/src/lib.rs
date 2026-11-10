// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Minimal ELF64 parser.
//!
//! Parses exactly what is needed to load a statically linked little-endian
//! ELF64 image: the file header and the program header table. Everything is
//! bounds-checked up front by [`Elf64::parse`], so iterating segments and
//! fetching their file contents afterwards cannot fail or panic.
//!
//! The crate contains no `unsafe` code and is unit-tested on the host. It is
//! used by the UEFI loader to load the kernel and is intended to be reused by
//! the kernel's userspace program loader.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

/// `e_machine` value for x86-64.
pub const EM_X86_64: u16 = 62;
/// `e_type` value for a fixed-address executable.
pub const ET_EXEC: u16 = 2;
/// `e_type` value for a position-independent executable or shared object.
pub const ET_DYN: u16 = 3;
/// `p_type` value for a loadable segment.
pub const PT_LOAD: u32 = 1;
/// Segment flag: executable.
pub const PF_X: u32 = 1;
/// Segment flag: writable.
pub const PF_W: u32 = 2;
/// Segment flag: readable.
pub const PF_R: u32 = 4;

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

/// Why an image was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// The file is shorter than a structure it claims to contain.
    Truncated,
    /// The file does not start with `\x7fELF`.
    BadMagic,
    /// Not a 64-bit ELF.
    NotElf64,
    /// Not little-endian.
    NotLittleEndian,
    /// Unknown ELF version.
    BadVersion,
    /// `e_phentsize` is not the ELF64 program header size.
    BadProgramHeaderSize,
    /// Program header `index` is inconsistent (its file range lies outside
    /// the file, `p_filesz > p_memsz`, or its memory range overflows).
    BadSegment {
        /// Offending program header.
        index: usize,
    },
}

impl core::fmt::Display for ElfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "ELF file is truncated"),
            Self::BadMagic => write!(f, "not an ELF file"),
            Self::NotElf64 => write!(f, "not a 64-bit ELF file"),
            Self::NotLittleEndian => write!(f, "not a little-endian ELF file"),
            Self::BadVersion => write!(f, "unsupported ELF version"),
            Self::BadProgramHeaderSize => write!(f, "unexpected program header size"),
            Self::BadSegment { index } => write!(f, "program header {index} is inconsistent"),
        }
    }
}

/// One entry of the program header table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramHeader {
    /// Segment type (`PT_*`).
    pub p_type: u32,
    /// Permission flags (`PF_*`).
    pub flags: u32,
    /// Offset of the segment contents in the file.
    pub offset: u64,
    /// Virtual address of the segment in memory.
    pub vaddr: u64,
    /// Number of bytes present in the file.
    pub filesz: u64,
    /// Number of bytes occupied in memory (`>= filesz`; the excess is
    /// zero-filled).
    pub memsz: u64,
    /// Required alignment.
    pub align: u64,
}

impl ProgramHeader {
    /// Whether this is a `PT_LOAD` segment.
    pub fn is_load(&self) -> bool {
        self.p_type == PT_LOAD
    }

    /// Exclusive end of the segment in memory. Cannot overflow for headers
    /// obtained from a successfully parsed [`Elf64`].
    pub fn vaddr_end(&self) -> u64 {
        self.vaddr + self.memsz
    }

    /// Whether the segment is mapped writable.
    pub fn is_writable(&self) -> bool {
        self.flags & PF_W != 0
    }

    /// Whether the segment is mapped executable.
    pub fn is_executable(&self) -> bool {
        self.flags & PF_X != 0
    }
}

/// A validated ELF64 image borrowed from a byte slice.
#[derive(Debug, Clone, Copy)]
pub struct Elf64<'a> {
    data: &'a [u8],
    elf_type: u16,
    machine: u16,
    entry: u64,
    phoff: usize,
    phnum: usize,
}

fn u16_at(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

fn u32_at(data: &[u8], off: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&data[off..off + 4]);
    u32::from_le_bytes(b)
}

fn u64_at(data: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[off..off + 8]);
    u64::from_le_bytes(b)
}

impl<'a> Elf64<'a> {
    /// Validates the file header and every program header.
    pub fn parse(data: &'a [u8]) -> Result<Self, ElfError> {
        if data.len() < EHDR_SIZE {
            return Err(if data.len() >= 4 && data[..4] != *b"\x7fELF" {
                ElfError::BadMagic
            } else {
                ElfError::Truncated
            });
        }
        if data[..4] != *b"\x7fELF" {
            return Err(ElfError::BadMagic);
        }
        if data[4] != 2 {
            return Err(ElfError::NotElf64);
        }
        if data[5] != 1 {
            return Err(ElfError::NotLittleEndian);
        }
        if data[6] != 1 || u32_at(data, 20) != 1 {
            return Err(ElfError::BadVersion);
        }

        let phnum = usize::from(u16_at(data, 56));
        let phoff = usize::try_from(u64_at(data, 32)).map_err(|_| ElfError::Truncated)?;
        if phnum != 0 {
            if usize::from(u16_at(data, 54)) != PHDR_SIZE {
                return Err(ElfError::BadProgramHeaderSize);
            }
            let table_end = phoff
                .checked_add(phnum * PHDR_SIZE)
                .ok_or(ElfError::Truncated)?;
            if table_end > data.len() {
                return Err(ElfError::Truncated);
            }
        }

        let elf = Self {
            data,
            elf_type: u16_at(data, 16),
            machine: u16_at(data, 18),
            entry: u64_at(data, 24),
            phoff,
            phnum,
        };

        for index in 0..phnum {
            let ph = elf.program_header(index);
            let file_end = ph.offset.checked_add(ph.filesz);
            let ok = ph.filesz <= ph.memsz
                && ph.vaddr.checked_add(ph.memsz).is_some()
                && file_end.is_some_and(|end| end <= data.len() as u64);
            if !ok {
                return Err(ElfError::BadSegment { index });
            }
        }
        Ok(elf)
    }

    /// `e_type` (`ET_*`).
    pub fn elf_type(&self) -> u16 {
        self.elf_type
    }

    /// `e_machine` (`EM_*`).
    pub fn machine(&self) -> u16 {
        self.machine
    }

    /// Virtual address of the entry point.
    pub fn entry(&self) -> u64 {
        self.entry
    }

    fn program_header(&self, index: usize) -> ProgramHeader {
        let ph = &self.data[self.phoff + index * PHDR_SIZE..][..PHDR_SIZE];
        ProgramHeader {
            p_type: u32_at(ph, 0),
            flags: u32_at(ph, 4),
            offset: u64_at(ph, 8),
            vaddr: u64_at(ph, 16),
            filesz: u64_at(ph, 32),
            memsz: u64_at(ph, 40),
            align: u64_at(ph, 48),
        }
    }

    /// Iterates over all program headers.
    pub fn program_headers(&self) -> impl Iterator<Item = ProgramHeader> + '_ {
        (0..self.phnum).map(|i| self.program_header(i))
    }

    /// Iterates over the `PT_LOAD` program headers.
    pub fn load_segments(&self) -> impl Iterator<Item = ProgramHeader> + '_ {
        self.program_headers().filter(ProgramHeader::is_load)
    }

    /// The bytes of `ph` that are present in the file (`p_filesz` bytes).
    ///
    /// `ph` must come from this image; for a foreign header whose range does
    /// not fit, an empty slice is returned.
    pub fn segment_data(&self, ph: &ProgramHeader) -> &'a [u8] {
        let start = usize::try_from(ph.offset).ok();
        let len = usize::try_from(ph.filesz).ok();
        match (start, len) {
            (Some(start), Some(len)) => start
                .checked_add(len)
                .and_then(|end| self.data.get(start..end))
                .unwrap_or(&[]),
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    struct Seg {
        p_type: u32,
        flags: u32,
        vaddr: u64,
        data: &'static [u8],
        memsz: u64,
    }

    /// Builds a well-formed ELF64 executable containing `segs`.
    fn build(segs: &[Seg]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x7fELF");
        out.extend_from_slice(&[2, 1, 1, 0]); // 64-bit, LE, v1, SysV
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&ET_EXEC.to_le_bytes());
        out.extend_from_slice(&EM_X86_64.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0xFFFF_FFFF_8000_1000u64.to_le_bytes()); // entry
        out.extend_from_slice(&(EHDR_SIZE as u64).to_le_bytes()); // phoff
        out.extend_from_slice(&0u64.to_le_bytes()); // shoff
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&(EHDR_SIZE as u16).to_le_bytes());
        out.extend_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        out.extend_from_slice(&(segs.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0; 6]); // shentsize, shnum, shstrndx
        assert_eq!(out.len(), EHDR_SIZE);

        let mut offset = (EHDR_SIZE + segs.len() * PHDR_SIZE) as u64;
        for s in segs {
            out.extend_from_slice(&s.p_type.to_le_bytes());
            out.extend_from_slice(&s.flags.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&s.vaddr.to_le_bytes());
            out.extend_from_slice(&s.vaddr.to_le_bytes()); // paddr
            out.extend_from_slice(&(s.data.len() as u64).to_le_bytes());
            out.extend_from_slice(&s.memsz.to_le_bytes());
            out.extend_from_slice(&0x1000u64.to_le_bytes());
            offset += s.data.len() as u64;
        }
        for s in segs {
            out.extend_from_slice(s.data);
        }
        out
    }

    fn sample() -> Vec<u8> {
        build(&[
            Seg {
                p_type: PT_LOAD,
                flags: PF_R | PF_X,
                vaddr: 0xFFFF_FFFF_8000_0000,
                data: b"\x90\x90\xc3",
                memsz: 3,
            },
            Seg {
                p_type: 4, // PT_NOTE
                flags: PF_R,
                vaddr: 0,
                data: b"note",
                memsz: 4,
            },
            Seg {
                p_type: PT_LOAD,
                flags: PF_R | PF_W,
                vaddr: 0xFFFF_FFFF_8000_2000,
                data: b"data",
                memsz: 0x3000,
            },
        ])
    }

    #[test]
    fn parses_header_and_segments() {
        let img = sample();
        let elf = Elf64::parse(&img).unwrap();
        assert_eq!(elf.elf_type(), ET_EXEC);
        assert_eq!(elf.machine(), EM_X86_64);
        assert_eq!(elf.entry(), 0xFFFF_FFFF_8000_1000);
        assert_eq!(elf.program_headers().count(), 3);

        let loads: Vec<_> = elf.load_segments().collect();
        assert_eq!(loads.len(), 2);
        assert!(loads[0].is_executable() && !loads[0].is_writable());
        assert!(loads[1].is_writable() && !loads[1].is_executable());
        assert_eq!(elf.segment_data(&loads[0]), b"\x90\x90\xc3");
        assert_eq!(elf.segment_data(&loads[1]), b"data");
        assert_eq!(loads[1].memsz, 0x3000);
        assert_eq!(loads[1].vaddr_end(), 0xFFFF_FFFF_8000_5000);
    }

    #[test]
    fn rejects_malformed_headers() {
        let img = sample();
        assert_eq!(Elf64::parse(&[]).unwrap_err(), ElfError::Truncated);
        assert_eq!(Elf64::parse(&img[..63]).unwrap_err(), ElfError::Truncated);
        assert_eq!(Elf64::parse(b"MZ\x90\x00").unwrap_err(), ElfError::BadMagic);

        let mut bad = img.clone();
        bad[0] = 0;
        assert_eq!(Elf64::parse(&bad).unwrap_err(), ElfError::BadMagic);
        let mut bad = img.clone();
        bad[4] = 1;
        assert_eq!(Elf64::parse(&bad).unwrap_err(), ElfError::NotElf64);
        let mut bad = img.clone();
        bad[5] = 2;
        assert_eq!(Elf64::parse(&bad).unwrap_err(), ElfError::NotLittleEndian);
        let mut bad = img.clone();
        bad[20] = 9;
        assert_eq!(Elf64::parse(&bad).unwrap_err(), ElfError::BadVersion);
        let mut bad = img.clone();
        bad[54] = 32;
        assert_eq!(
            Elf64::parse(&bad).unwrap_err(),
            ElfError::BadProgramHeaderSize
        );
    }

    #[test]
    fn rejects_out_of_bounds_tables_and_segments() {
        let img = sample();

        // Program header table runs past the end of the file.
        assert_eq!(
            Elf64::parse(&img[..EHDR_SIZE + PHDR_SIZE]).unwrap_err(),
            ElfError::Truncated
        );
        let mut bad = img.clone();
        bad[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(Elf64::parse(&bad).unwrap_err(), ElfError::Truncated);

        // Segment contents cut off.
        assert_eq!(
            Elf64::parse(&img[..img.len() - 1]).unwrap_err(),
            ElfError::BadSegment { index: 2 }
        );

        // filesz > memsz on the first segment.
        let ph0 = EHDR_SIZE;
        let mut bad = img.clone();
        bad[ph0 + 40..ph0 + 48].copy_from_slice(&1u64.to_le_bytes());
        assert_eq!(
            Elf64::parse(&bad).unwrap_err(),
            ElfError::BadSegment { index: 0 }
        );

        // offset + filesz overflows.
        let mut bad = img.clone();
        bad[ph0 + 8..ph0 + 16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            Elf64::parse(&bad).unwrap_err(),
            ElfError::BadSegment { index: 0 }
        );

        // vaddr + memsz overflows.
        let mut bad = img.clone();
        bad[ph0 + 16..ph0 + 24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            Elf64::parse(&bad).unwrap_err(),
            ElfError::BadSegment { index: 0 }
        );
    }

    #[test]
    fn image_without_program_headers_is_valid_but_empty() {
        let img = build(&[]);
        let elf = Elf64::parse(&img).unwrap();
        assert_eq!(elf.load_segments().count(), 0);
    }

    #[test]
    fn foreign_header_yields_empty_data() {
        let img = sample();
        let elf = Elf64::parse(&img).unwrap();
        let foreign = ProgramHeader {
            p_type: PT_LOAD,
            flags: PF_R,
            offset: u64::MAX,
            vaddr: 0,
            filesz: 16,
            memsz: 16,
            align: 1,
        };
        assert!(elf.segment_data(&foreign).is_empty());
    }
}
