// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Creating ext2 filesystems.

use alloc::vec;
use alloc::vec::Vec;

use crate::{
    BlockIo, DIRENT_DIRECTORY, Ext2Error, FEATURE_INCOMPAT_FILETYPE, GROUP_DESCRIPTOR_SIZE, MAGIC,
    MODE_DIRECTORY, ROOT_INODE, SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE, put16, put32,
};

/// Block size of filesystems created by [`format`].
const BLOCK_SIZE: usize = 4096;
const INODE_SIZE: usize = 128;
/// First inode available for ordinary files; 1-10 are reserved.
const FIRST_INODE: u32 = 11;
const LOST_AND_FOUND_INODE: u32 = 11;
/// One inode is provided for this many bytes of space.
const BYTES_PER_INODE: u64 = 16 * 1024;

/// Where one block group keeps its metadata.
struct Layout {
    start: u32,
    blocks: u32,
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    /// First block available for data.
    data: u32,
}

fn set_bits(bitmap: &mut [u8], range: core::ops::Range<usize>) {
    for bit in range {
        bitmap[bit / 8] |= 1 << (bit % 8);
    }
}

/// Writes a directory block containing `.`, `..` and optionally one more
/// entry.
fn directory_block(own: u32, parent: u32, extra: Option<(&str, u32)>) -> Vec<u8> {
    let mut data = vec![0u8; BLOCK_SIZE];
    let mut entry = |offset: usize, inode: u32, record: usize, name: &str| {
        put32(&mut data, offset, inode);
        put16(&mut data, offset + 4, record as u16);
        data[offset + 6] = name.len() as u8;
        data[offset + 7] = DIRENT_DIRECTORY;
        data[offset + 8..offset + 8 + name.len()].copy_from_slice(name.as_bytes());
    };
    entry(0, own, 12, ".");
    match extra {
        Some((name, inode)) => {
            entry(12, parent, 12, "..");
            entry(24, inode, BLOCK_SIZE - 24, name);
        }
        None => entry(12, parent, BLOCK_SIZE - 12, ".."),
    }
    data
}

/// Creates an empty ext2 filesystem covering `device`, destroying whatever
/// it contained. The result has a root directory and `lost+found`, uses
/// 4 KiB blocks and 128-byte inodes, and passes `e2fsck`.
pub fn format<D: BlockIo>(device: &mut D) -> Result<(), Ext2Error> {
    let io = |_| Ext2Error::Io;
    let blocks_per_group = (8 * BLOCK_SIZE) as u32;
    let mut total = u32::try_from(device.size() / BLOCK_SIZE as u64).unwrap_or(u32::MAX);

    // Inodes per group: by the bytes-per-inode ratio, rounded up to fill
    // whole inode-table blocks, and at least one block's worth.
    let inodes_per_block = (BLOCK_SIZE / INODE_SIZE) as u32;
    let group_bytes = u64::from(blocks_per_group.min(total)) * BLOCK_SIZE as u64;
    let inodes_per_group = ((group_bytes / BYTES_PER_INODE) as u32)
        .clamp(inodes_per_block, blocks_per_group)
        .next_multiple_of(inodes_per_block);
    let table_blocks = inodes_per_group / inodes_per_block;

    // Every group starts with a superblock copy and the group descriptor
    // table (no `sparse_super`), then its bitmaps and inode table.
    let plan = |total: u32| {
        let groups = total.div_ceil(blocks_per_group);
        let descriptor_blocks =
            (groups as usize * GROUP_DESCRIPTOR_SIZE).div_ceil(BLOCK_SIZE) as u32;
        (
            groups,
            descriptor_blocks,
            1 + descriptor_blocks + 2 + table_blocks,
        )
    };
    // A trailing group too small to hold its own metadata plus some data
    // is left out.
    let (mut groups, mut descriptor_blocks, mut overhead) = plan(total);
    let tail = total - (groups.max(1) - 1) * blocks_per_group;
    if groups > 1 && tail < overhead + 8 {
        total -= tail;
        (groups, descriptor_blocks, overhead) = plan(total);
    }
    // Root directory and lost+found each need a block.
    if groups == 0 || total < overhead + 16 {
        return Err(Ext2Error::TooSmall);
    }

    let layouts: Vec<Layout> = (0..groups)
        .map(|group| {
            let start = group * blocks_per_group;
            let block_bitmap = start + 1 + descriptor_blocks;
            Layout {
                start,
                blocks: (total - start).min(blocks_per_group),
                block_bitmap,
                inode_bitmap: block_bitmap + 1,
                inode_table: block_bitmap + 2,
                data: start + overhead,
            }
        })
        .collect();

    let root_block = layouts[0].data;
    let lost_block = root_block + 1;
    let write_block = |device: &mut D, block: u32, data: &[u8]| {
        device
            .write(u64::from(block) * BLOCK_SIZE as u64, data)
            .map_err(io)
    };

    // Per-group bitmaps and inode tables.
    let mut descriptors = vec![0u8; descriptor_blocks as usize * BLOCK_SIZE];
    let mut free_blocks_total = 0u32;
    let mut free_inodes_total = 0u32;
    for (index, layout) in layouts.iter().enumerate() {
        let mut used_blocks = overhead;
        let mut used_inodes = 0;
        let mut directories = 0;

        let mut block_bitmap = vec![0u8; BLOCK_SIZE];
        set_bits(&mut block_bitmap, 0..overhead as usize);
        // Bits past the end of a short last group are marked in use.
        set_bits(&mut block_bitmap, layout.blocks as usize..8 * BLOCK_SIZE);
        let mut inode_bitmap = vec![0u8; BLOCK_SIZE];
        set_bits(&mut inode_bitmap, inodes_per_group as usize..8 * BLOCK_SIZE);
        if index == 0 {
            set_bits(&mut block_bitmap, overhead as usize..overhead as usize + 2);
            used_blocks += 2;
            // Reserved inodes 1-10 and lost+found.
            set_bits(&mut inode_bitmap, 0..LOST_AND_FOUND_INODE as usize);
            used_inodes = LOST_AND_FOUND_INODE;
            directories = 2;
        }
        write_block(device, layout.block_bitmap, &block_bitmap)?;
        write_block(device, layout.inode_bitmap, &inode_bitmap)?;

        let mut table = vec![0u8; table_blocks as usize * BLOCK_SIZE];
        if index == 0 {
            let mut directory_inode = |number: u32, links: u16, block: u32| {
                let inode = &mut table[(number as usize - 1) * INODE_SIZE..][..INODE_SIZE];
                put16(inode, 0, MODE_DIRECTORY | 0o755);
                put32(inode, 4, BLOCK_SIZE as u32);
                put16(inode, 26, links);
                put32(inode, 28, (BLOCK_SIZE / 512) as u32);
                put32(inode, 40, block);
            };
            // Root: `.`, its own `..`, and lost+found's `..`.
            directory_inode(ROOT_INODE, 3, root_block);
            directory_inode(LOST_AND_FOUND_INODE, 2, lost_block);
        }
        write_block(device, layout.inode_table, &table)?;

        let free_blocks = layout.blocks - used_blocks;
        let free_inodes = inodes_per_group - used_inodes;
        free_blocks_total += free_blocks;
        free_inodes_total += free_inodes;
        let descriptor = &mut descriptors[index * GROUP_DESCRIPTOR_SIZE..];
        put32(descriptor, 0, layout.block_bitmap);
        put32(descriptor, 4, layout.inode_bitmap);
        put32(descriptor, 8, layout.inode_table);
        put16(descriptor, 12, free_blocks as u16);
        put16(descriptor, 14, free_inodes as u16);
        put16(descriptor, 16, directories);
    }

    write_block(
        device,
        root_block,
        &directory_block(
            ROOT_INODE,
            ROOT_INODE,
            Some(("lost+found", LOST_AND_FOUND_INODE)),
        ),
    )?;
    write_block(
        device,
        lost_block,
        &directory_block(LOST_AND_FOUND_INODE, ROOT_INODE, None),
    )?;

    let mut superblock = vec![0u8; SUPERBLOCK_SIZE];
    put32(&mut superblock, 0, inodes_per_group * groups);
    put32(&mut superblock, 4, total);
    put32(&mut superblock, 12, free_blocks_total);
    put32(&mut superblock, 16, free_inodes_total);
    put32(&mut superblock, 20, 0); // first data block (block size > 1 KiB)
    put32(&mut superblock, 24, 2); // log2(block size) - 10
    put32(&mut superblock, 28, 2); // fragment size, same as block size
    put32(&mut superblock, 32, blocks_per_group);
    put32(&mut superblock, 36, blocks_per_group); // fragments per group
    put32(&mut superblock, 40, inodes_per_group);
    put16(&mut superblock, 54, u16::MAX); // no maximum mount count
    put16(&mut superblock, 56, MAGIC);
    put16(&mut superblock, 58, 1); // state: cleanly unmounted
    put16(&mut superblock, 60, 1); // on errors: continue
    put32(&mut superblock, 76, 1); // revision 1 (dynamic inode sizes)
    put32(&mut superblock, 84, FIRST_INODE);
    put16(&mut superblock, 88, INODE_SIZE as u16);
    put32(&mut superblock, 96, FEATURE_INCOMPAT_FILETYPE);
    // A fixed identifier rather than a random one: there is no entropy
    // source yet, and identical inputs should give identical images.
    superblock[104..120].copy_from_slice(b"OmitOS-ext2-vol1");
    superblock[120..127].copy_from_slice(b"omit-os");

    // Backup copies in every group but the first, each recording its own
    // group number, then the primary last so that a half-finished format
    // is not mistaken for a filesystem.
    for (index, layout) in layouts.iter().enumerate().skip(1) {
        let mut copy = vec![0u8; BLOCK_SIZE];
        copy[..SUPERBLOCK_SIZE].copy_from_slice(&superblock);
        put16(&mut copy, 90, index as u16);
        write_block(device, layout.start, &copy)?;
        device
            .write(
                u64::from(layout.start + 1) * BLOCK_SIZE as u64,
                &descriptors,
            )
            .map_err(io)?;
    }
    device.write(BLOCK_SIZE as u64, &descriptors).map_err(io)?;
    device.write(SUPERBLOCK_OFFSET, &superblock).map_err(io)
}
