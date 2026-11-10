// SPDX-License-Identifier: GPL-2.0-only
//! ext2 filesystem driver.
//!
//! Reads and writes the second extended filesystem (revision 1) on any
//! [`BlockIo`] device, implements the VFS [`FileSystem`] interface on top
//! of it, and can create new filesystems ([`format`]).
//!
//! ext2 was chosen as the first persistent filesystem because it is simple,
//! thoroughly documented, and — decisively — can be checked with tools that
//! are independent of this code: the test suite runs `e2fsck` over images
//! this driver has written and exchanges files with `debugfs`.
//!
//! # Supported
//!
//! Regular files and directories; block sizes of 1, 2 and 4 KiB; direct,
//! single- and double-indirect blocks (files up to about 4 GiB with 4 KiB
//! blocks); sparse files; the `filetype`, `sparse_super` and `large_file`
//! features.
//!
//! # Not supported
//!
//! Journaling (this is ext2, not ext3/4): a crash in the middle of an
//! operation can leave the filesystem inconsistent, and `e2fsck` is then
//! needed. Symbolic links, device nodes, permissions and ownership (stored
//! but not interpreted), timestamps (written as zero), extended attributes,
//! triple-indirect blocks, and any other feature flag: filesystems using
//! unknown incompatible features are refused.
//!
//! All metadata is written through synchronously; there is no cache.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod format;
#[cfg(test)]
mod tests;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use omit_vfs::path::validate_name;
use omit_vfs::{DirEntry, FileSystem, FileType, FsError, FsResult, InodeId, Metadata};

pub use format::format;

/// A random-access storage device addressed in bytes.
pub trait BlockIo {
    /// Size of the device in bytes.
    fn size(&self) -> u64;
    /// Reads `buffer.len()` bytes at `offset`.
    fn read(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), IoError>;
    /// Writes `data` at `offset`.
    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), IoError>;
}

/// The device failed to complete a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoError;

/// Why a filesystem could not be mounted or created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ext2Error {
    /// The device failed.
    Io,
    /// There is no ext2 superblock.
    NotExt2,
    /// The filesystem uses features this driver does not implement.
    UnsupportedFeatures(u32),
    /// The superblock or group descriptors are inconsistent.
    Corrupt(&'static str),
    /// The device is too small to hold a filesystem.
    TooSmall,
}

impl core::fmt::Display for Ext2Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io => write!(f, "device I/O error"),
            Self::NotExt2 => write!(f, "not an ext2 filesystem"),
            Self::UnsupportedFeatures(bits) => {
                write!(f, "unsupported filesystem features ({bits:#x})")
            }
            Self::Corrupt(what) => write!(f, "corrupt filesystem: {what}"),
            Self::TooSmall => write!(f, "device is too small"),
        }
    }
}

impl From<IoError> for FsError {
    fn from(_: IoError) -> Self {
        Self::Io
    }
}

const SUPERBLOCK_OFFSET: u64 = 1024;
const SUPERBLOCK_SIZE: usize = 1024;
const MAGIC: u16 = 0xEF53;
const ROOT_INODE: u32 = 2;

const FEATURE_INCOMPAT_FILETYPE: u32 = 0x2;
const FEATURE_RO_SPARSE_SUPER: u32 = 0x1;
const FEATURE_RO_LARGE_FILE: u32 = 0x2;

const MODE_TYPE_MASK: u16 = 0xF000;
const MODE_DIRECTORY: u16 = 0x4000;
const MODE_REGULAR: u16 = 0x8000;

const DIRENT_FILE: u8 = 1;
const DIRENT_DIRECTORY: u8 = 2;

/// Deletion time recorded for removed inodes. There is no wall clock yet,
/// so a fixed date (2026-01-01 00:00:00 UTC) is used.
const DELETION_TIME: u32 = 1_767_225_600;

const DIRECT_BLOCKS: u64 = 12;
const GROUP_DESCRIPTOR_SIZE: usize = 32;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Rounded-up length of a directory entry holding a name of `name_len`.
fn dirent_len(name_len: usize) -> usize {
    (8 + name_len).next_multiple_of(4)
}

/// An in-memory copy of an on-disk inode.
#[derive(Clone)]
struct Inode {
    number: u32,
    /// The raw on-disk bytes (`inode_size` of them); fields not understood
    /// by this driver are preserved.
    raw: Vec<u8>,
}

impl Inode {
    fn mode(&self) -> u16 {
        le16(&self.raw, 0)
    }

    fn is_dir(&self) -> bool {
        self.mode() & MODE_TYPE_MASK == MODE_DIRECTORY
    }

    fn size(&self) -> u64 {
        let low = u64::from(le32(&self.raw, 4));
        if self.is_dir() {
            low
        } else {
            low | (u64::from(le32(&self.raw, 108)) << 32)
        }
    }

    fn set_size(&mut self, size: u64) {
        put32(&mut self.raw, 4, size as u32);
        if !self.is_dir() {
            put32(&mut self.raw, 108, (size >> 32) as u32);
        }
    }

    fn links(&self) -> u16 {
        le16(&self.raw, 26)
    }

    fn set_links(&mut self, links: u16) {
        put16(&mut self.raw, 26, links);
    }

    /// Allocated size in 512-byte units, as ext2 counts it.
    fn sectors(&self) -> u32 {
        le32(&self.raw, 28)
    }

    fn set_sectors(&mut self, sectors: u32) {
        put32(&mut self.raw, 28, sectors);
    }

    fn pointer(&self, index: usize) -> u32 {
        le32(&self.raw, 40 + 4 * index)
    }

    fn set_pointer(&mut self, index: usize, block: u32) {
        put32(&mut self.raw, 40 + 4 * index, block);
    }
}

#[derive(Clone, Copy)]
struct Group {
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    free_blocks: u16,
    free_inodes: u16,
    used_dirs: u16,
}

/// The mounted filesystem's state. All methods take `&mut self`; the public
/// wrapper provides interior mutability for the VFS's `&self` methods.
struct Inner<D: BlockIo> {
    device: D,
    /// Raw superblock; counters are patched in before it is written back.
    superblock: Vec<u8>,
    groups: Vec<Group>,
    block_size: usize,
    blocks_count: u32,
    first_data_block: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: usize,
    free_blocks: u32,
    free_inodes: u32,
    has_filetype: bool,
}

impl<D: BlockIo> Inner<D> {
    fn mount(mut device: D) -> Result<Self, Ext2Error> {
        let mut superblock = vec![0u8; SUPERBLOCK_SIZE];
        device
            .read(SUPERBLOCK_OFFSET, &mut superblock)
            .map_err(|_| Ext2Error::Io)?;
        if le16(&superblock, 56) != MAGIC {
            return Err(Ext2Error::NotExt2);
        }
        let incompat = le32(&superblock, 96);
        if incompat & !FEATURE_INCOMPAT_FILETYPE != 0 {
            return Err(Ext2Error::UnsupportedFeatures(incompat));
        }
        // Unknown read-only-compatible features make writing unsafe; this
        // driver always mounts read-write.
        let ro_compat = le32(&superblock, 100);
        if ro_compat & !(FEATURE_RO_SPARSE_SUPER | FEATURE_RO_LARGE_FILE) != 0 {
            return Err(Ext2Error::UnsupportedFeatures(ro_compat));
        }

        let log_block_size = le32(&superblock, 24);
        if log_block_size > 2 {
            return Err(Ext2Error::Corrupt("unsupported block size"));
        }
        let block_size = 1024usize << log_block_size;
        let revision = le32(&superblock, 76);
        let inode_size = if revision >= 1 {
            usize::from(le16(&superblock, 88))
        } else {
            128
        };
        let blocks_count = le32(&superblock, 4);
        let first_data_block = le32(&superblock, 20);
        let blocks_per_group = le32(&superblock, 32);
        let inodes_per_group = le32(&superblock, 40);
        if inode_size < 128
            || inode_size > block_size
            || !inode_size.is_power_of_two()
            || blocks_per_group == 0
            || blocks_per_group as usize > 8 * block_size
            || inodes_per_group == 0
            || inodes_per_group as usize > 8 * block_size
            || blocks_count <= first_data_block
            || u64::from(blocks_count) * block_size as u64 > device.size()
        {
            return Err(Ext2Error::Corrupt("superblock geometry"));
        }

        let group_count = (blocks_count - first_data_block).div_ceil(blocks_per_group) as usize;
        let mut filesystem = Self {
            device,
            free_blocks: le32(&superblock, 12),
            free_inodes: le32(&superblock, 16),
            superblock,
            groups: Vec::new(),
            block_size,
            blocks_count,
            first_data_block,
            blocks_per_group,
            inodes_per_group,
            inode_size,
            has_filetype: incompat & FEATURE_INCOMPAT_FILETYPE != 0,
        };

        let table_block = first_data_block + 1;
        let table_bytes = group_count * GROUP_DESCRIPTOR_SIZE;
        let mut table = vec![0u8; table_bytes.next_multiple_of(block_size)];
        filesystem
            .device
            .read(u64::from(table_block) * block_size as u64, &mut table)
            .map_err(|_| Ext2Error::Io)?;
        for index in 0..group_count {
            let d = &table[index * GROUP_DESCRIPTOR_SIZE..];
            let group = Group {
                block_bitmap: le32(d, 0),
                inode_bitmap: le32(d, 4),
                inode_table: le32(d, 8),
                free_blocks: le16(d, 12),
                free_inodes: le16(d, 14),
                used_dirs: le16(d, 16),
            };
            if [group.block_bitmap, group.inode_bitmap, group.inode_table]
                .iter()
                .any(|&b| b <= first_data_block || b >= blocks_count)
            {
                return Err(Ext2Error::Corrupt("group descriptor"));
            }
            filesystem.groups.push(group);
        }
        Ok(filesystem)
    }

    // --- Block I/O ------------------------------------------------------

    fn read_block(&mut self, block: u32) -> FsResult<Vec<u8>> {
        if block >= self.blocks_count {
            return Err(FsError::Io);
        }
        let mut data = vec![0u8; self.block_size];
        self.device
            .read(u64::from(block) * self.block_size as u64, &mut data)?;
        Ok(data)
    }

    fn write_block(&mut self, block: u32, data: &[u8]) -> FsResult<()> {
        if block >= self.blocks_count || block <= self.first_data_block {
            // Never overwrite the superblock or anything before it.
            return Err(FsError::Io);
        }
        self.device
            .write(u64::from(block) * self.block_size as u64, data)?;
        Ok(())
    }

    /// Writes the superblock and group descriptor table with the current
    /// counters.
    fn flush_metadata(&mut self) -> FsResult<()> {
        put32(&mut self.superblock, 12, self.free_blocks);
        put32(&mut self.superblock, 16, self.free_inodes);
        let superblock = self.superblock.clone();
        self.device.write(SUPERBLOCK_OFFSET, &superblock)?;

        let table_bytes = self.groups.len() * GROUP_DESCRIPTOR_SIZE;
        let table_offset = u64::from(self.first_data_block + 1) * self.block_size as u64;
        let mut table = vec![0u8; table_bytes.next_multiple_of(self.block_size)];
        self.device.read(table_offset, &mut table)?;
        for (index, group) in self.groups.iter().enumerate() {
            let d = &mut table[index * GROUP_DESCRIPTOR_SIZE..];
            put16(d, 12, group.free_blocks);
            put16(d, 14, group.free_inodes);
            put16(d, 16, group.used_dirs);
        }
        self.device.write(table_offset, &table)?;
        Ok(())
    }

    // --- Allocation -----------------------------------------------------

    fn blocks_in_group(&self, group: usize) -> u32 {
        let start = self.first_data_block + group as u32 * self.blocks_per_group;
        (self.blocks_count - start).min(self.blocks_per_group)
    }

    /// Allocates a zero-filled block, preferring group `goal`.
    fn alloc_block(&mut self, goal: usize) -> FsResult<u32> {
        let count = self.groups.len();
        for step in 0..count {
            let group = (goal + step) % count;
            if self.groups[group].free_blocks == 0 {
                continue;
            }
            let bitmap_block = self.groups[group].block_bitmap;
            let mut bitmap = self.read_block(bitmap_block)?;
            let limit = self.blocks_in_group(group) as usize;
            let Some(bit) = (0..limit).find(|&bit| bitmap[bit / 8] & (1 << (bit % 8)) == 0) else {
                continue;
            };
            bitmap[bit / 8] |= 1 << (bit % 8);
            self.write_block(bitmap_block, &bitmap)?;
            self.groups[group].free_blocks -= 1;
            self.free_blocks = self.free_blocks.saturating_sub(1);

            let block = self.first_data_block + group as u32 * self.blocks_per_group + bit as u32;
            let zeros = vec![0u8; self.block_size];
            self.write_block(block, &zeros)?;
            return Ok(block);
        }
        Err(FsError::NoSpace)
    }

    fn free_block(&mut self, block: u32) -> FsResult<()> {
        if block <= self.first_data_block || block >= self.blocks_count {
            return Err(FsError::Io);
        }
        let relative = block - self.first_data_block;
        let group = (relative / self.blocks_per_group) as usize;
        let bit = (relative % self.blocks_per_group) as usize;
        let bitmap_block = self.groups[group].block_bitmap;
        let mut bitmap = self.read_block(bitmap_block)?;
        if bitmap[bit / 8] & (1 << (bit % 8)) != 0 {
            bitmap[bit / 8] &= !(1 << (bit % 8));
            self.write_block(bitmap_block, &bitmap)?;
            self.groups[group].free_blocks += 1;
            self.free_blocks += 1;
        }
        Ok(())
    }

    fn alloc_inode(&mut self, goal: usize, directory: bool) -> FsResult<u32> {
        let count = self.groups.len();
        for step in 0..count {
            let group = (goal + step) % count;
            if self.groups[group].free_inodes == 0 {
                continue;
            }
            let bitmap_block = self.groups[group].inode_bitmap;
            let mut bitmap = self.read_block(bitmap_block)?;
            let limit = self.inodes_per_group as usize;
            let Some(bit) = (0..limit).find(|&bit| bitmap[bit / 8] & (1 << (bit % 8)) == 0) else {
                continue;
            };
            bitmap[bit / 8] |= 1 << (bit % 8);
            self.write_block(bitmap_block, &bitmap)?;
            self.groups[group].free_inodes -= 1;
            self.free_inodes = self.free_inodes.saturating_sub(1);
            if directory {
                self.groups[group].used_dirs += 1;
            }
            return Ok(group as u32 * self.inodes_per_group + bit as u32 + 1);
        }
        Err(FsError::NoSpace)
    }

    fn free_inode(&mut self, number: u32, directory: bool) -> FsResult<()> {
        let group = ((number - 1) / self.inodes_per_group) as usize;
        let bit = ((number - 1) % self.inodes_per_group) as usize;
        let bitmap_block = self.groups[group].inode_bitmap;
        let mut bitmap = self.read_block(bitmap_block)?;
        bitmap[bit / 8] &= !(1 << (bit % 8));
        self.write_block(bitmap_block, &bitmap)?;
        self.groups[group].free_inodes += 1;
        self.free_inodes += 1;
        if directory {
            self.groups[group].used_dirs = self.groups[group].used_dirs.saturating_sub(1);
        }
        Ok(())
    }

    // --- Inodes ---------------------------------------------------------

    fn inode_location(&self, number: u32) -> FsResult<(u32, usize)> {
        let total = self.inodes_per_group as u64 * self.groups.len() as u64;
        if number == 0 || u64::from(number) > total {
            return Err(FsError::NotFound);
        }
        let group = ((number - 1) / self.inodes_per_group) as usize;
        let index = ((number - 1) % self.inodes_per_group) as usize;
        let offset = index * self.inode_size;
        Ok((
            self.groups[group].inode_table + (offset / self.block_size) as u32,
            offset % self.block_size,
        ))
    }

    fn read_inode(&mut self, number: u32) -> FsResult<Inode> {
        let (block, offset) = self.inode_location(number)?;
        let data = self.read_block(block)?;
        Ok(Inode {
            number,
            raw: data[offset..offset + self.inode_size].to_vec(),
        })
    }

    /// Like [`Inner::read_inode`], but fails for inodes that are not in
    /// use (link count zero), so stale inode numbers are not followed.
    fn live_inode(&mut self, number: InodeId) -> FsResult<Inode> {
        let number = u32::try_from(number).map_err(|_| FsError::NotFound)?;
        let inode = self.read_inode(number)?;
        if inode.links() == 0 || inode.mode() == 0 {
            return Err(FsError::NotFound);
        }
        Ok(inode)
    }

    fn write_inode(&mut self, inode: &Inode) -> FsResult<()> {
        let (block, offset) = self.inode_location(inode.number)?;
        let mut data = self.read_block(block)?;
        data[offset..offset + self.inode_size].copy_from_slice(&inode.raw);
        self.write_block(block, &data)
    }

    fn group_of_inode(&self, number: u32) -> usize {
        ((number - 1) / self.inodes_per_group) as usize
    }

    // --- Block mapping --------------------------------------------------

    fn pointers_per_block(&self) -> u64 {
        (self.block_size / 4) as u64
    }

    fn charge(&self, inode: &mut Inode, blocks: i64) {
        let sectors = (self.block_size / 512) as i64 * blocks;
        inode.set_sectors((i64::from(inode.sectors()) + sectors).max(0) as u32);
    }

    /// Returns the entry at `index` of the pointer block `table`,
    /// allocating a block for it if it is empty and `allocate` is set.
    fn follow(
        &mut self,
        inode: &mut Inode,
        table: u32,
        index: u64,
        allocate: bool,
    ) -> FsResult<Option<u32>> {
        let mut data = self.read_block(table)?;
        let at = index as usize * 4;
        let existing = le32(&data, at);
        if existing != 0 || !allocate {
            return Ok((existing != 0).then_some(existing));
        }
        let block = self.alloc_block(self.group_of_inode(inode.number))?;
        put32(&mut data, at, block);
        self.write_block(table, &data)?;
        self.charge(inode, 1);
        Ok(Some(block))
    }

    /// Like `follow`, for one of the pointers stored in the inode itself.
    fn follow_inode(
        &mut self,
        inode: &mut Inode,
        slot: usize,
        allocate: bool,
    ) -> FsResult<Option<u32>> {
        let existing = inode.pointer(slot);
        if existing != 0 || !allocate {
            return Ok((existing != 0).then_some(existing));
        }
        let block = self.alloc_block(self.group_of_inode(inode.number))?;
        inode.set_pointer(slot, block);
        self.charge(inode, 1);
        Ok(Some(block))
    }

    /// Maps file block `index` of `inode` to a filesystem block. With
    /// `allocate`, missing blocks (including pointer blocks) are created
    /// and `inode` is updated in memory; the caller writes it back.
    fn map_block(
        &mut self,
        inode: &mut Inode,
        index: u64,
        allocate: bool,
    ) -> FsResult<Option<u32>> {
        let per_block = self.pointers_per_block();
        if index < DIRECT_BLOCKS {
            return self.follow_inode(inode, index as usize, allocate);
        }
        let index = index - DIRECT_BLOCKS;
        if index < per_block {
            let Some(single) = self.follow_inode(inode, 12, allocate)? else {
                return Ok(None);
            };
            return self.follow(inode, single, index, allocate);
        }
        let index = index - per_block;
        if index < per_block * per_block {
            let Some(double) = self.follow_inode(inode, 13, allocate)? else {
                return Ok(None);
            };
            let Some(single) = self.follow(inode, double, index / per_block, allocate)? else {
                return Ok(None);
            };
            return self.follow(inode, single, index % per_block, allocate);
        }
        // Triple-indirect blocks are not implemented.
        Err(FsError::NoSpace)
    }

    /// Frees the file blocks at index `keep` and beyond that are reachable
    /// through the pointer block `table` (`level` 1 = pointers to data).
    /// `first` is the file block index of the table's first entry. Returns
    /// whether the table is now empty (and may itself be freed).
    fn free_tree(
        &mut self,
        inode: &mut Inode,
        table: u32,
        level: u32,
        first: u64,
        keep: u64,
    ) -> FsResult<bool> {
        let per_block = self.pointers_per_block();
        let span = per_block.pow(level - 1);
        let mut data = self.read_block(table)?;
        let mut changed = false;
        let mut empty = true;
        for slot in 0..per_block {
            let at = slot as usize * 4;
            let block = le32(&data, at);
            if block == 0 {
                continue;
            }
            let start = first + slot * span;
            let release = if level == 1 {
                start >= keep
            } else if start + span <= keep {
                false
            } else {
                self.free_tree(inode, block, level - 1, start, keep)?
            };
            if release {
                self.free_block(block)?;
                self.charge(inode, -1);
                put32(&mut data, at, 0);
                changed = true;
            } else {
                empty = false;
            }
        }
        if changed && !empty {
            self.write_block(table, &data)?;
        }
        Ok(empty)
    }

    /// Frees every block of `inode` at file block index `keep` and beyond.
    fn free_blocks_from(&mut self, inode: &mut Inode, keep: u64) -> FsResult<()> {
        let per_block = self.pointers_per_block();
        for slot in keep.min(DIRECT_BLOCKS)..DIRECT_BLOCKS {
            let block = inode.pointer(slot as usize);
            if block != 0 {
                self.free_block(block)?;
                self.charge(inode, -1);
                inode.set_pointer(slot as usize, 0);
            }
        }
        let levels = [
            (12usize, 1u32, DIRECT_BLOCKS),
            (13, 2, DIRECT_BLOCKS + per_block),
        ];
        for (slot, level, first) in levels {
            let table = inode.pointer(slot);
            if table != 0 && self.free_tree(inode, table, level, first, keep)? {
                self.free_block(table)?;
                self.charge(inode, -1);
                inode.set_pointer(slot, 0);
            }
        }
        Ok(())
    }

    // --- File contents --------------------------------------------------

    fn read_at(&mut self, inode: &mut Inode, offset: u64, buffer: &mut [u8]) -> FsResult<usize> {
        let size = inode.size();
        if offset >= size {
            return Ok(0);
        }
        let total = buffer.len().min((size - offset) as usize);
        let block_size = self.block_size as u64;
        let mut done = 0;
        while done < total {
            let position = offset + done as u64;
            let within = (position % block_size) as usize;
            let chunk = (self.block_size - within).min(total - done);
            match self.map_block(inode, position / block_size, false)? {
                Some(block) => {
                    let data = self.read_block(block)?;
                    buffer[done..done + chunk].copy_from_slice(&data[within..within + chunk]);
                }
                // A hole reads as zeros.
                None => buffer[done..done + chunk].fill(0),
            }
            done += chunk;
        }
        Ok(total)
    }

    /// Writes `data` at `offset`, growing the file as needed, and writes
    /// the inode back.
    fn write_at(&mut self, inode: &mut Inode, offset: u64, data: &[u8]) -> FsResult<usize> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(FsError::NoSpace)?;
        let block_size = self.block_size as u64;
        let mut done = 0;
        let mut result = Ok(());
        while done < data.len() {
            let position = offset + done as u64;
            let within = (position % block_size) as usize;
            let chunk = (self.block_size - within).min(data.len() - done);
            let block = match self.map_block(inode, position / block_size, true) {
                Ok(block) => block.ok_or(FsError::Io)?,
                Err(error) => {
                    result = Err(error);
                    break;
                }
            };
            if chunk == self.block_size {
                self.write_block(block, &data[done..done + chunk])?;
            } else {
                let mut existing = self.read_block(block)?;
                existing[within..within + chunk].copy_from_slice(&data[done..done + chunk]);
                self.write_block(block, &existing)?;
            }
            done += chunk;
        }
        // Record what was written even if space ran out part-way, so the
        // blocks allocated so far are accounted for.
        let written_end = offset + done as u64;
        if written_end > inode.size() && done > 0 {
            inode.set_size(written_end);
        }
        self.write_inode(inode)?;
        self.flush_metadata()?;
        result?;
        debug_assert_eq!(written_end, end);
        Ok(done)
    }

    fn truncate(&mut self, inode: &mut Inode, size: u64) -> FsResult<()> {
        let block_size = self.block_size as u64;
        if size < inode.size() {
            self.free_blocks_from(inode, size.div_ceil(block_size))?;
            // Zero the remainder of the last block that is kept, so the
            // old contents do not reappear if the file grows again.
            let within = (size % block_size) as usize;
            if within != 0
                && let Some(block) = self.map_block(inode, size / block_size, false)?
            {
                let mut data = self.read_block(block)?;
                data[within..].fill(0);
                self.write_block(block, &data)?;
            }
        }
        inode.set_size(size);
        self.write_inode(inode)?;
        self.flush_metadata()
    }

    // --- Directories ----------------------------------------------------

    /// Calls `visit(block index, offset, inode, record length, type, name)`
    /// for every directory entry, including unused ones (inode 0). Stops
    /// early if `visit` returns `true`.
    fn scan_directory(
        &mut self,
        directory: &mut Inode,
        mut visit: impl FnMut(u64, usize, u32, usize, u8, &[u8]) -> bool,
    ) -> FsResult<()> {
        let blocks = directory.size() / self.block_size as u64;
        for index in 0..blocks {
            let Some(block) = self.map_block(directory, index, false)? else {
                continue;
            };
            let data = self.read_block(block)?;
            let mut offset = 0;
            while offset + 8 <= self.block_size {
                let record = usize::from(le16(&data, offset + 4));
                let name_len = usize::from(data[offset + 6]);
                if record < 8 || offset + record > self.block_size || 8 + name_len > record {
                    return Err(FsError::Io);
                }
                let name = &data[offset + 8..offset + 8 + name_len];
                if visit(
                    index,
                    offset,
                    le32(&data, offset),
                    record,
                    data[offset + 7],
                    name,
                ) {
                    return Ok(());
                }
                offset += record;
            }
        }
        Ok(())
    }

    fn find_entry(&mut self, directory: &mut Inode, name: &str) -> FsResult<Option<u32>> {
        let mut found = None;
        self.scan_directory(directory, |_, _, inode, _, _, entry_name| {
            if inode != 0 && entry_name == name.as_bytes() {
                found = Some(inode);
            }
            found.is_some()
        })?;
        Ok(found)
    }

    /// Adds an entry to `directory` and writes the directory inode back if
    /// it grew.
    fn add_entry(
        &mut self,
        directory: &mut Inode,
        name: &str,
        inode: u32,
        file_type: FileType,
    ) -> FsResult<()> {
        let needed = dirent_len(name.len());
        let type_byte = match (self.has_filetype, file_type) {
            (false, _) => 0,
            (true, FileType::File) => DIRENT_FILE,
            (true, FileType::Directory) => DIRENT_DIRECTORY,
        };

        // Look for an entry with enough slack after its own contents.
        let mut slot = None;
        self.scan_directory(
            directory,
            |index, offset, entry_inode, record, _, entry_name| {
                let used = if entry_inode == 0 {
                    0
                } else {
                    dirent_len(entry_name.len())
                };
                if record - used >= needed {
                    slot = Some((index, offset, used, record));
                }
                slot.is_some()
            },
        )?;

        let (block, mut data, offset, record) = match slot {
            Some((index, offset, used, record)) => {
                let block = self
                    .map_block(directory, index, false)?
                    .ok_or(FsError::Io)?;
                let mut data = self.read_block(block)?;
                if used != 0 {
                    // Shrink the existing entry to what it needs.
                    put16(&mut data, offset + 4, used as u16);
                }
                (block, data, offset + used, record - used)
            }
            None => {
                let index = directory.size() / self.block_size as u64;
                let block = self.map_block(directory, index, true)?.ok_or(FsError::Io)?;
                directory.set_size(directory.size() + self.block_size as u64);
                self.write_inode(directory)?;
                (block, vec![0u8; self.block_size], 0, self.block_size)
            }
        };
        put32(&mut data, offset, inode);
        put16(&mut data, offset + 4, record as u16);
        data[offset + 6] = name.len() as u8;
        data[offset + 7] = type_byte;
        data[offset + 8..offset + 8 + name.len()].copy_from_slice(name.as_bytes());
        self.write_block(block, &data)
    }

    fn remove_entry(&mut self, directory: &mut Inode, name: &str) -> FsResult<()> {
        // Find the entry and the one before it in the same block.
        let mut target = None;
        let mut previous: Option<(u64, usize)> = None;
        self.scan_directory(directory, |index, offset, inode, record, _, entry_name| {
            if inode != 0 && entry_name == name.as_bytes() {
                let before = previous
                    .filter(|&(block, _)| block == index)
                    .map(|(_, o)| o);
                target = Some((index, offset, record, before));
                return true;
            }
            previous = Some((index, offset));
            false
        })?;
        let (index, offset, record, before) = target.ok_or(FsError::NotFound)?;
        let block = self
            .map_block(directory, index, false)?
            .ok_or(FsError::Io)?;
        let mut data = self.read_block(block)?;
        match before {
            // Fold the space into the preceding entry.
            Some(previous) => {
                let merged = usize::from(le16(&data, previous + 4)) + record;
                put16(&mut data, previous + 4, merged as u16);
            }
            // First entry of a block: mark it unused.
            None => put32(&mut data, offset, 0),
        }
        self.write_block(block, &data)
    }

    fn set_parent(&mut self, directory: &mut Inode, parent: u32) -> FsResult<()> {
        let mut location = None;
        self.scan_directory(directory, |index, offset, inode, _, _, name| {
            if inode != 0 && name == b".." {
                location = Some((index, offset));
            }
            location.is_some()
        })?;
        let (index, offset) = location.ok_or(FsError::Io)?;
        let block = self
            .map_block(directory, index, false)?
            .ok_or(FsError::Io)?;
        let mut data = self.read_block(block)?;
        put32(&mut data, offset, parent);
        self.write_block(block, &data)
    }

    /// Names and inodes of the real entries (not `.` or `..`).
    fn list(&mut self, directory: &mut Inode) -> FsResult<Vec<(String, u32)>> {
        let mut entries = Vec::new();
        let mut bad_name = false;
        self.scan_directory(directory, |_, _, inode, _, _, name| {
            if inode != 0 && name != b"." && name != b".." {
                match core::str::from_utf8(name) {
                    Ok(name) => entries.push((String::from(name), inode)),
                    // Names that are not UTF-8 cannot be represented.
                    Err(_) => bad_name = true,
                }
            }
            false
        })?;
        let _ = bad_name;
        Ok(entries)
    }

    fn directory(&mut self, number: InodeId) -> FsResult<Inode> {
        let inode = self.live_inode(number)?;
        if inode.is_dir() {
            Ok(inode)
        } else {
            Err(FsError::NotADirectory)
        }
    }

    fn file(&mut self, number: InodeId) -> FsResult<Inode> {
        let inode = self.live_inode(number)?;
        if inode.is_dir() {
            Err(FsError::IsADirectory)
        } else {
            Ok(inode)
        }
    }

    // --- Operations -----------------------------------------------------

    fn create(&mut self, parent: InodeId, name: &str, file_type: FileType) -> FsResult<InodeId> {
        validate_name(name)?;
        let mut directory = self.directory(parent)?;
        if self.find_entry(&mut directory, name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let is_dir = file_type == FileType::Directory;
        if is_dir && directory.links() == u16::MAX {
            return Err(FsError::NoSpace);
        }

        let number = self.alloc_inode(self.group_of_inode(directory.number), is_dir)?;
        let mut inode = Inode {
            number,
            raw: vec![0u8; self.inode_size],
        };
        put16(
            &mut inode.raw,
            0,
            if is_dir {
                MODE_DIRECTORY | 0o755
            } else {
                MODE_REGULAR | 0o644
            },
        );
        inode.set_links(if is_dir { 2 } else { 1 });

        let result = (|| {
            if is_dir {
                let block = self.map_block(&mut inode, 0, true)?.ok_or(FsError::Io)?;
                let mut data = vec![0u8; self.block_size];
                let type_byte = if self.has_filetype {
                    DIRENT_DIRECTORY
                } else {
                    0
                };
                put32(&mut data, 0, number);
                put16(&mut data, 4, 12);
                data[6] = 1;
                data[7] = type_byte;
                data[8] = b'.';
                put32(&mut data, 12, directory.number);
                put16(&mut data, 16, (self.block_size - 12) as u16);
                data[18] = 2;
                data[19] = type_byte;
                data[20..22].copy_from_slice(b"..");
                self.write_block(block, &data)?;
                inode.set_size(self.block_size as u64);
            }
            self.write_inode(&inode)?;
            self.add_entry(&mut directory, name, number, file_type)
        })();
        if let Err(error) = result {
            // Undo the allocations so a full disk does not leak them.
            let _ = self.free_blocks_from(&mut inode, 0);
            let _ = self.free_inode(number, is_dir);
            let _ = self.flush_metadata();
            return Err(error);
        }

        if is_dir {
            // The new directory's `..` refers to the parent.
            directory.set_links(directory.links() + 1);
            self.write_inode(&directory)?;
        }
        self.flush_metadata()?;
        Ok(InodeId::from(number))
    }

    fn remove(&mut self, parent: InodeId, name: &str) -> FsResult<()> {
        let mut directory = self.directory(parent)?;
        let number = self
            .find_entry(&mut directory, name)?
            .ok_or(FsError::NotFound)?;
        let mut inode = self.read_inode(number)?;
        let is_dir = inode.is_dir();
        if is_dir && !self.list(&mut inode)?.is_empty() {
            return Err(FsError::DirectoryNotEmpty);
        }

        self.remove_entry(&mut directory, name)?;
        if is_dir {
            directory.set_links(directory.links().saturating_sub(1));
            self.write_inode(&directory)?;
        }
        let links = if is_dir {
            0
        } else {
            inode.links().saturating_sub(1)
        };
        inode.set_links(links);
        if links == 0 {
            self.free_blocks_from(&mut inode, 0)?;
            inode.set_size(0);
            // A deletion time marks the inode as deleted. It must look like
            // a timestamp: small values are read by e2fsck as a link in
            // the orphan list, which shares this field.
            put32(&mut inode.raw, 20, DELETION_TIME);
            self.write_inode(&inode)?;
            self.free_inode(number, is_dir)?;
        } else {
            self.write_inode(&inode)?;
        }
        self.flush_metadata()
    }

    fn rename(
        &mut self,
        from_parent: InodeId,
        from_name: &str,
        to_parent: InodeId,
        to_name: &str,
    ) -> FsResult<()> {
        validate_name(to_name)?;
        let mut from_directory = self.directory(from_parent)?;
        let number = self
            .find_entry(&mut from_directory, from_name)?
            .ok_or(FsError::NotFound)?;
        let mut to_directory = self.directory(to_parent)?;
        if self.find_entry(&mut to_directory, to_name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        let mut inode = self.read_inode(number)?;
        let is_dir = inode.is_dir();

        if is_dir {
            // A directory must not be moved into itself or a descendant:
            // walk from the destination up to the root.
            let mut cursor = to_directory.number;
            loop {
                if cursor == number {
                    return Err(FsError::InvalidArgument);
                }
                if cursor == ROOT_INODE {
                    break;
                }
                let mut current = self.read_inode(cursor)?;
                cursor = self.find_entry(&mut current, "..")?.ok_or(FsError::Io)?;
            }
        }

        let file_type = if is_dir {
            FileType::Directory
        } else {
            FileType::File
        };
        self.add_entry(&mut to_directory, to_name, number, file_type)?;
        // Re-read: with both names in one directory, `add_entry` may have
        // changed it.
        let same_directory = from_directory.number == to_directory.number;
        if same_directory {
            from_directory = self.read_inode(from_directory.number)?;
        }
        self.remove_entry(&mut from_directory, from_name)?;

        if is_dir && !same_directory {
            self.set_parent(&mut inode, to_directory.number)?;
            let mut from_directory = self.read_inode(from_directory.number)?;
            from_directory.set_links(from_directory.links().saturating_sub(1));
            self.write_inode(&from_directory)?;
            let mut to_directory = self.read_inode(to_directory.number)?;
            to_directory.set_links(to_directory.links() + 1);
            self.write_inode(&to_directory)?;
        }
        self.flush_metadata()
    }
}

/// A mounted ext2 filesystem.
pub struct Ext2<D: BlockIo> {
    inner: RefCell<Inner<D>>,
}

impl<D: BlockIo> Ext2<D> {
    /// Mounts the filesystem on `device`, read-write.
    pub fn mount(device: D) -> Result<Self, Ext2Error> {
        Ok(Self {
            inner: RefCell::new(Inner::mount(device)?),
        })
    }

    /// Unmounts and returns the device. Everything has already been
    /// written: the driver does not cache.
    pub fn into_device(self) -> D {
        self.inner.into_inner().device
    }

    /// Free space in bytes.
    pub fn free_bytes(&self) -> u64 {
        let inner = self.inner.borrow();
        u64::from(inner.free_blocks) * inner.block_size as u64
    }

    /// Total size in bytes.
    pub fn total_bytes(&self) -> u64 {
        let inner = self.inner.borrow();
        u64::from(inner.blocks_count) * inner.block_size as u64
    }
}

impl<D: BlockIo + Send> FileSystem for Ext2<D> {
    fn root(&self) -> InodeId {
        InodeId::from(ROOT_INODE)
    }

    fn metadata(&self, inode: InodeId) -> FsResult<Metadata> {
        let mut inner = self.inner.borrow_mut();
        let mut node = inner.live_inode(inode)?;
        let (file_type, size) = if node.is_dir() {
            (FileType::Directory, inner.list(&mut node)?.len() as u64)
        } else {
            (FileType::File, node.size())
        };
        Ok(Metadata {
            inode,
            file_type,
            size,
        })
    }

    fn lookup(&self, dir: InodeId, name: &str) -> FsResult<InodeId> {
        let mut inner = self.inner.borrow_mut();
        let mut directory = inner.directory(dir)?;
        inner
            .find_entry(&mut directory, name)?
            .map(InodeId::from)
            .ok_or(FsError::NotFound)
    }

    fn read_dir(&self, dir: InodeId) -> FsResult<Vec<DirEntry>> {
        let mut inner = self.inner.borrow_mut();
        let mut directory = inner.directory(dir)?;
        let mut entries = Vec::new();
        for (name, number) in inner.list(&mut directory)? {
            let file_type = if inner.read_inode(number)?.is_dir() {
                FileType::Directory
            } else {
                FileType::File
            };
            entries.push(DirEntry {
                name,
                inode: InodeId::from(number),
                file_type,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn create(&mut self, dir: InodeId, name: &str, file_type: FileType) -> FsResult<InodeId> {
        self.inner.get_mut().create(dir, name, file_type)
    }

    fn remove(&mut self, dir: InodeId, name: &str) -> FsResult<()> {
        self.inner.get_mut().remove(dir, name)
    }

    fn rename(
        &mut self,
        from_dir: InodeId,
        from_name: &str,
        to_dir: InodeId,
        to_name: &str,
    ) -> FsResult<()> {
        self.inner
            .get_mut()
            .rename(from_dir, from_name, to_dir, to_name)
    }

    fn read(&self, inode: InodeId, offset: u64, buffer: &mut [u8]) -> FsResult<usize> {
        let mut inner = self.inner.borrow_mut();
        let mut node = inner.file(inode)?;
        inner.read_at(&mut node, offset, buffer)
    }

    fn write(&mut self, inode: InodeId, offset: u64, data: &[u8]) -> FsResult<usize> {
        let inner = self.inner.get_mut();
        let mut node = inner.file(inode)?;
        inner.write_at(&mut node, offset, data)
    }

    fn truncate(&mut self, inode: InodeId, size: u64) -> FsResult<()> {
        let inner = self.inner.get_mut();
        let mut node = inner.file(inode)?;
        inner.truncate(&mut node, size)
    }
}
