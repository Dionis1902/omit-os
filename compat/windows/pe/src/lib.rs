// SPDX-License-Identifier: GPL-2.0-only
//! PE32+ executables: the file format of 64-bit Windows programs.
//!
//! This crate reads an executable from a byte slice, validates it, and
//! provides the three things a loader needs:
//!
//! * [`Pe::build_image`] lays the headers and sections out in memory the
//!   way the file describes;
//! * [`Pe::relocations`] lists the places that hold absolute addresses and
//!   must be adjusted when the image is not loaded at its preferred
//!   address;
//! * [`Pe::imports`] lists the functions the program expects the system to
//!   provide.
//!
//! Everything is written from the public format description ("PE Format",
//! Microsoft Learn). The parser never trusts the file: every offset and
//! size is checked, and all arithmetic is overflow-checked. It contains no
//! `unsafe` code.
//!
//! Not supported, and reported as errors or through [`Pe::features`]
//! rather than ignored silently: 32-bit images, other CPU architectures,
//! sections aligned to less than a page, and relocation types other than
//! the one x86-64 uses.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

use core::fmt;

/// Size of a page; sections must be aligned to it so that each can get its
/// own access rights.
pub const PAGE_SIZE: u32 = 4096;

/// `IMAGE_FILE_MACHINE_AMD64`.
pub const MACHINE_AMD64: u16 = 0x8664;
/// The subsystem of a graphical program.
pub const SUBSYSTEM_GUI: u16 = 2;
/// The subsystem of a console program.
pub const SUBSYSTEM_CONSOLE: u16 = 3;

const FILE_RELOCS_STRIPPED: u16 = 0x0001;
const FILE_EXECUTABLE_IMAGE: u16 = 0x0002;
const FILE_DLL: u16 = 0x2000;

const SECTION_EXECUTE: u32 = 0x2000_0000;
const SECTION_WRITE: u32 = 0x8000_0000;

const DIRECTORY_IMPORT: usize = 1;
const DIRECTORY_BASE_RELOCATION: usize = 5;
const DIRECTORY_TLS: usize = 9;
const DIRECTORY_DELAY_IMPORT: usize = 13;
const DIRECTORY_CLR: usize = 14;

const RELOCATION_ABSOLUTE: u16 = 0;
const RELOCATION_DIR64: u16 = 10;

/// Most sections accepted; real programs have a handful.
pub const MAX_SECTIONS: usize = 96;

/// Why a file was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeError {
    /// The file ends before a structure it claims to contain.
    Truncated,
    /// No `MZ` signature: not a DOS/Windows executable at all.
    NotMz,
    /// No `PE\0\0` signature (for example a plain DOS program).
    NotPe,
    /// The program is for another CPU; the value is the machine type.
    WrongMachine(u16),
    /// Not a PE32+ image; the value is the optional-header magic
    /// (`0x10B` is a 32-bit program).
    NotPe32Plus(u16),
    /// The file is not marked as an executable image (an object file).
    NotExecutable,
    /// A header field has a value the format does not allow.
    BadHeader(&'static str),
    /// A section lies outside the file or the image, or overlaps another.
    BadSection,
    /// A relative virtual address points outside the image's data.
    BadRva(u32),
    /// The import tables are malformed.
    BadImport,
    /// The relocation table is malformed.
    BadRelocation,
    /// A relocation type this loader does not implement.
    UnsupportedRelocation(u16),
    /// The buffer passed to [`Pe::build_image`] has the wrong size.
    WrongImageSize,
}

impl fmt::Display for PeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "file is truncated"),
            Self::NotMz => write!(f, "not an executable (no MZ signature)"),
            Self::NotPe => write!(f, "not a Windows executable (no PE signature)"),
            Self::WrongMachine(0x014C) => write!(f, "32-bit x86 programs are not supported"),
            Self::WrongMachine(0xAA64) => write!(f, "ARM64 programs are not supported"),
            Self::WrongMachine(machine) => {
                write!(
                    f,
                    "program is for another CPU (machine type {machine:#06x})"
                )
            }
            Self::NotPe32Plus(0x010B) => write!(f, "32-bit (PE32) programs are not supported"),
            Self::NotPe32Plus(magic) => write!(f, "unknown image format {magic:#06x}"),
            Self::NotExecutable => write!(f, "file is not an executable image"),
            Self::BadHeader(what) => write!(f, "invalid header: {what}"),
            Self::BadSection => write!(f, "invalid section table"),
            Self::BadRva(rva) => write!(f, "address {rva:#x} is outside the image"),
            Self::BadImport => write!(f, "invalid import table"),
            Self::BadRelocation => write!(f, "invalid relocation table"),
            Self::UnsupportedRelocation(kind) => {
                write!(f, "unsupported relocation type {kind}")
            }
            Self::WrongImageSize => write!(f, "image buffer has the wrong size"),
        }
    }
}

/// Result type of this crate.
pub type PeResult<T> = Result<T, PeError>;

fn bytes(data: &[u8], offset: usize, length: usize) -> PeResult<&[u8]> {
    let end = offset.checked_add(length).ok_or(PeError::Truncated)?;
    data.get(offset..end).ok_or(PeError::Truncated)
}

fn read_u16(data: &[u8], offset: usize) -> PeResult<u16> {
    let raw = bytes(data, offset, 2)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> PeResult<u32> {
    let raw = bytes(data, offset, 4)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u64(data: &[u8], offset: usize) -> PeResult<u64> {
    let raw = bytes(data, offset, 8)?;
    let mut value = [0u8; 8];
    value.copy_from_slice(raw);
    Ok(u64::from_le_bytes(value))
}

/// One section of the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    name: [u8; 8],
    /// Address of the section relative to the image base.
    pub rva: u32,
    /// Size of the section in memory.
    pub virtual_size: u32,
    /// Offset of the section's initialised data in the file.
    pub file_offset: u32,
    /// Number of bytes of initialised data; the rest of the section, up to
    /// `virtual_size`, is zero.
    pub file_size: u32,
    /// Whether the section holds code.
    pub executable: bool,
    /// Whether the program may write to the section.
    pub writable: bool,
}

impl Section {
    /// The section's name, such as `.text`.
    pub fn name(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(8);
        core::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }

    /// Number of bytes copied from the file: no more than fits in memory.
    fn copied(&self) -> u32 {
        self.file_size.min(self.virtual_size)
    }
}

/// Parts of the format an image uses that a loader has to know about,
/// because ignoring them would make the program misbehave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Features {
    /// The image has thread-local storage (data or callbacks).
    pub tls: bool,
    /// The image has delay-loaded imports.
    pub delay_imports: bool,
    /// The image is a .NET program: its code is not machine code.
    pub dotnet: bool,
}

/// A function the program imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Import<'a> {
    /// The library that provides it, such as `KERNEL32.dll`. Windows
    /// compares these names case-insensitively.
    pub library: &'a str,
    /// How the function is identified.
    pub symbol: Symbol<'a>,
    /// Where in the image the function's address must be stored (an
    /// 8-byte slot of the import address table).
    pub slot_rva: u32,
}

/// The identification of an imported function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symbol<'a> {
    /// By name.
    Name(&'a str),
    /// By number within the library.
    Ordinal(u16),
}

impl fmt::Display for Symbol<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => write!(f, "{name}"),
            Self::Ordinal(ordinal) => write!(f, "#{ordinal}"),
        }
    }
}

/// A validated PE32+ executable.
#[derive(Debug, Clone)]
pub struct Pe<'a> {
    data: &'a [u8],
    characteristics: u16,
    entry_rva: u32,
    image_base: u64,
    size_of_image: u32,
    size_of_headers: u32,
    subsystem: u16,
    stack_reserve: u64,
    directories: [(u32, u32); 16],
    sections: [Section; MAX_SECTIONS],
    section_count: usize,
}

impl<'a> Pe<'a> {
    /// Whether `data` starts like a DOS/Windows executable. Cheap check
    /// for deciding how to run a file.
    pub fn is_mz(data: &[u8]) -> bool {
        data.starts_with(b"MZ")
    }

    /// Parses and validates the executable in `data`.
    pub fn parse(data: &'a [u8]) -> PeResult<Self> {
        if !Self::is_mz(data) {
            return Err(PeError::NotMz);
        }
        // A file too short to hold the DOS header's pointer to the PE
        // header is a DOS program at best.
        let pe_offset = read_u32(data, 0x3C).map_err(|_| PeError::NotPe)? as usize;
        if bytes(data, pe_offset, 4).map_err(|_| PeError::NotPe)? != b"PE\0\0" {
            return Err(PeError::NotPe);
        }

        let coff = pe_offset + 4;
        let machine = read_u16(data, coff)?;
        if machine != MACHINE_AMD64 {
            return Err(PeError::WrongMachine(machine));
        }
        let section_count = usize::from(read_u16(data, coff + 2)?);
        let optional_size = usize::from(read_u16(data, coff + 16)?);
        let characteristics = read_u16(data, coff + 18)?;
        if characteristics & FILE_EXECUTABLE_IMAGE == 0 {
            return Err(PeError::NotExecutable);
        }

        let optional = coff + 20;
        let magic = read_u16(data, optional)?;
        if magic != 0x020B {
            return Err(PeError::NotPe32Plus(magic));
        }
        if optional_size < 112 {
            return Err(PeError::BadHeader("optional header too small"));
        }
        // Everything below must lie inside the optional header.
        let header = bytes(data, optional, optional_size)?;
        let entry_rva = read_u32(header, 16)?;
        let image_base = read_u64(header, 24)?;
        let section_alignment = read_u32(header, 32)?;
        let file_alignment = read_u32(header, 36)?;
        let size_of_image = read_u32(header, 56)?;
        let size_of_headers = read_u32(header, 60)?;
        let subsystem = read_u16(header, 68)?;
        let stack_reserve = read_u64(header, 72)?;
        let directory_count = read_u32(header, 108)? as usize;

        if !section_alignment.is_power_of_two() || section_alignment < PAGE_SIZE {
            return Err(PeError::BadHeader("sections are not page-aligned"));
        }
        if !file_alignment.is_power_of_two() || file_alignment > section_alignment {
            return Err(PeError::BadHeader("invalid file alignment"));
        }
        if size_of_image == 0 || !size_of_image.is_multiple_of(section_alignment) {
            return Err(PeError::BadHeader("invalid image size"));
        }
        if !image_base.is_multiple_of(0x1_0000) {
            return Err(PeError::BadHeader("image base is not a multiple of 64 KiB"));
        }
        if size_of_headers == 0
            || size_of_headers > size_of_image
            || size_of_headers as usize > data.len()
        {
            return Err(PeError::BadHeader("invalid header size"));
        }

        let mut directories = [(0u32, 0u32); 16];
        for (index, directory) in directories.iter_mut().enumerate().take(directory_count) {
            let at = 112 + index * 8;
            *directory = (read_u32(header, at)?, read_u32(header, at + 4)?);
        }

        if section_count > MAX_SECTIONS {
            return Err(PeError::BadSection);
        }
        let table = optional + optional_size;
        if table + section_count * 40 > size_of_headers as usize {
            return Err(PeError::BadSection);
        }
        let empty = Section {
            name: [0; 8],
            rva: 0,
            virtual_size: 0,
            file_offset: 0,
            file_size: 0,
            executable: false,
            writable: false,
        };
        let mut sections = [empty; MAX_SECTIONS];
        // Sections follow the headers and each other, in address order.
        let mut lowest = size_of_headers.next_multiple_of(section_alignment);
        for (index, section) in sections.iter_mut().enumerate().take(section_count) {
            let at = table + index * 40;
            let mut name = [0u8; 8];
            name.copy_from_slice(bytes(data, at, 8)?);
            let flags = read_u32(data, at + 36)?;
            let mut virtual_size = read_u32(data, at + 8)?;
            let file_size = read_u32(data, at + 16)?;
            if virtual_size == 0 {
                // Allowed by old linkers: the size in memory is the size
                // in the file.
                virtual_size = file_size;
            }
            *section = Section {
                name,
                rva: read_u32(data, at + 12)?,
                virtual_size,
                file_offset: read_u32(data, at + 20)?,
                file_size,
                executable: flags & SECTION_EXECUTE != 0,
                writable: flags & SECTION_WRITE != 0,
            };
            let end = section
                .rva
                .checked_add(section.virtual_size)
                .ok_or(PeError::BadSection)?;
            if section.rva < lowest
                || !section.rva.is_multiple_of(section_alignment)
                || end > size_of_image
            {
                return Err(PeError::BadSection);
            }
            lowest = end
                .checked_next_multiple_of(section_alignment)
                .ok_or(PeError::BadSection)?;
            // The initialised data must be in the file.
            bytes(
                data,
                section.file_offset as usize,
                section.copied() as usize,
            )
            .map_err(|_| PeError::BadSection)?;
        }

        let pe = Self {
            data,
            characteristics,
            entry_rva,
            image_base,
            size_of_image,
            size_of_headers,
            subsystem,
            stack_reserve,
            directories,
            sections,
            section_count,
        };
        if !pe.is_dll() || entry_rva != 0 {
            let in_code = pe.sections().iter().any(|section| {
                section.executable
                    && entry_rva >= section.rva
                    && entry_rva - section.rva < section.virtual_size
            });
            if !in_code {
                return Err(PeError::BadHeader("entry point is not in a code section"));
            }
        }
        Ok(pe)
    }

    /// Address of the first instruction, relative to the image base.
    pub fn entry_rva(&self) -> u32 {
        self.entry_rva
    }

    /// The address the image was linked for.
    pub fn image_base(&self) -> u64 {
        self.image_base
    }

    /// Size of the image in memory; a multiple of the page size.
    pub fn size_of_image(&self) -> u32 {
        self.size_of_image
    }

    /// Size of the headers at the start of the image.
    pub fn size_of_headers(&self) -> u32 {
        self.size_of_headers
    }

    /// The subsystem the program needs; see [`SUBSYSTEM_CONSOLE`].
    pub fn subsystem(&self) -> u16 {
        self.subsystem
    }

    /// Stack size the program asks for, in bytes.
    pub fn stack_reserve(&self) -> u64 {
        self.stack_reserve
    }

    /// Whether the file is a library rather than a program.
    pub fn is_dll(&self) -> bool {
        self.characteristics & FILE_DLL != 0
    }

    /// Whether the image can only run at [`Pe::image_base`] because its
    /// relocation information was removed.
    pub fn relocations_stripped(&self) -> bool {
        self.characteristics & FILE_RELOCS_STRIPPED != 0
    }

    /// The sections, in address order.
    pub fn sections(&self) -> &[Section] {
        &self.sections[..self.section_count]
    }

    /// Format features the image uses that need loader support.
    pub fn features(&self) -> Features {
        let present = |index: usize| self.directories[index] != (0, 0);
        Features {
            tls: present(DIRECTORY_TLS),
            delay_imports: present(DIRECTORY_DELAY_IMPORT),
            dotnet: present(DIRECTORY_CLR),
        }
    }

    /// Lays the image out in `image`, which must be exactly
    /// [`Pe::size_of_image`] bytes: headers first, then each section at
    /// its address, with everything else zero.
    pub fn build_image(&self, image: &mut [u8]) -> PeResult<()> {
        if image.len() != self.size_of_image as usize {
            return Err(PeError::WrongImageSize);
        }
        image.fill(0);
        let headers = self.size_of_headers as usize;
        image[..headers].copy_from_slice(&self.data[..headers]);
        for section in self.sections() {
            let length = section.copied() as usize;
            let from = section.file_offset as usize;
            let to = section.rva as usize;
            // Both ranges were validated by `parse`.
            image[to..to + length].copy_from_slice(&self.data[from..from + length]);
        }
        Ok(())
    }

    /// The initialised bytes of the image starting at `rva`, up to the end
    /// of the section (or headers) containing it.
    fn at(&self, rva: u32) -> PeResult<&'a [u8]> {
        if rva < self.size_of_headers {
            return Ok(&self.data[rva as usize..self.size_of_headers as usize]);
        }
        for section in self.sections() {
            if rva >= section.rva && rva - section.rva < section.copied() {
                let start = (section.file_offset + (rva - section.rva)) as usize;
                let end = (section.file_offset + section.copied()) as usize;
                return Ok(&self.data[start..end]);
            }
        }
        Err(PeError::BadRva(rva))
    }

    /// The NUL-terminated ASCII string at `rva`.
    fn string(&self, rva: u32) -> PeResult<&'a str> {
        let data = self.at(rva)?;
        let length = data
            .iter()
            .position(|&b| b == 0)
            .ok_or(PeError::BadImport)?;
        let text = core::str::from_utf8(&data[..length]).map_err(|_| PeError::BadImport)?;
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(PeError::BadImport);
        }
        Ok(text)
    }

    /// Calls `visit` for every function the image imports.
    pub fn imports(&self, mut visit: impl FnMut(Import<'a>)) -> PeResult<()> {
        let (table_rva, table_size) = self.directories[DIRECTORY_IMPORT];
        if table_rva == 0 || table_size == 0 {
            return Ok(());
        }
        let descriptors = self.at(table_rva).map_err(|_| PeError::BadImport)?;
        for descriptor in descriptors.chunks(20) {
            if descriptor.len() < 20 {
                // The table must end with an empty descriptor.
                return Err(PeError::BadImport);
            }
            if descriptor.iter().all(|&b| b == 0) {
                return Ok(());
            }
            let lookup_rva = read_u32(descriptor, 0)?;
            let name_rva = read_u32(descriptor, 12)?;
            let address_rva = read_u32(descriptor, 16)?;
            if address_rva == 0 || !address_rva.is_multiple_of(8) {
                return Err(PeError::BadImport);
            }
            let library = self.string(name_rva)?;
            // Without a separate lookup table, the address table itself
            // holds the names until the loader overwrites it.
            let lookup = if lookup_rva != 0 {
                lookup_rva
            } else {
                address_rva
            };
            let entries = self.at(lookup).map_err(|_| PeError::BadImport)?;
            let mut slot_rva = address_rva;
            let mut terminated = false;
            for entry in entries.chunks_exact(8) {
                let value = read_u64(entry, 0)?;
                if value == 0 {
                    terminated = true;
                    break;
                }
                let symbol = if value >> 63 != 0 {
                    Symbol::Ordinal(value as u16)
                } else {
                    let hint_name = u32::try_from(value).map_err(|_| PeError::BadImport)?;
                    // Two bytes of hint precede the name.
                    let name = hint_name.checked_add(2).ok_or(PeError::BadImport)?;
                    Symbol::Name(self.string(name)?)
                };
                self.check_slot(slot_rva)?;
                visit(Import {
                    library,
                    symbol,
                    slot_rva,
                });
                slot_rva = slot_rva.checked_add(8).ok_or(PeError::BadImport)?;
            }
            if !terminated {
                return Err(PeError::BadImport);
            }
        }
        Err(PeError::BadImport)
    }

    /// Checks that an 8-byte import slot lies inside a section.
    fn check_slot(&self, rva: u32) -> PeResult<()> {
        let inside = self.sections().iter().any(|section| {
            rva >= section.rva
                && u64::from(rva - section.rva) + 8 <= u64::from(section.virtual_size)
        });
        if inside {
            Ok(())
        } else {
            Err(PeError::BadImport)
        }
    }

    /// Calls `visit` with the address (relative to the image base) of
    /// every 64-bit absolute address stored in the image. When the image
    /// is loaded somewhere other than [`Pe::image_base`], the difference
    /// must be added to each of those values; see [`relocate`].
    pub fn relocations(&self, mut visit: impl FnMut(u32)) -> PeResult<()> {
        let (table_rva, table_size) = self.directories[DIRECTORY_BASE_RELOCATION];
        if table_rva == 0 || table_size == 0 {
            return Ok(());
        }
        let available = self.at(table_rva).map_err(|_| PeError::BadRelocation)?;
        let mut table = available
            .get(..table_size as usize)
            .ok_or(PeError::BadRelocation)?;
        while !table.is_empty() {
            let page_rva = read_u32(table, 0).map_err(|_| PeError::BadRelocation)?;
            let block_size = read_u32(table, 4).map_err(|_| PeError::BadRelocation)? as usize;
            if block_size < 8 || !block_size.is_multiple_of(4) || block_size > table.len() {
                return Err(PeError::BadRelocation);
            }
            for entry in table[8..block_size].chunks_exact(2) {
                let entry = u16::from_le_bytes([entry[0], entry[1]]);
                let offset = u32::from(entry & 0x0FFF);
                match entry >> 12 {
                    // Padding to keep blocks 4-byte aligned.
                    RELOCATION_ABSOLUTE => {}
                    RELOCATION_DIR64 => {
                        let rva = page_rva.checked_add(offset).ok_or(PeError::BadRelocation)?;
                        if u64::from(rva) + 8 > u64::from(self.size_of_image) {
                            return Err(PeError::BadRelocation);
                        }
                        visit(rva);
                    }
                    other => return Err(PeError::UnsupportedRelocation(other)),
                }
            }
            table = &table[block_size..];
        }
        Ok(())
    }
}

/// Adjusts the image built by [`Pe::build_image`] for being loaded at
/// address `base` instead of its preferred address.
pub fn relocate(pe: &Pe<'_>, image: &mut [u8], base: u64) -> PeResult<()> {
    if image.len() != pe.size_of_image() as usize {
        return Err(PeError::WrongImageSize);
    }
    let delta = base.wrapping_sub(pe.image_base());
    if delta == 0 {
        return Ok(());
    }
    pe.relocations(|rva| {
        // `relocations` only reports addresses inside the image.
        let slot = &mut image[rva as usize..rva as usize + 8];
        let mut value = [0u8; 8];
        value.copy_from_slice(slot);
        let adjusted = u64::from_le_bytes(value).wrapping_add(delta);
        slot.copy_from_slice(&adjusted.to_le_bytes());
    })
}
