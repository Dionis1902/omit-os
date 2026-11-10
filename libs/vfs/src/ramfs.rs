// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! In-memory filesystem.
//!
//! Files are sequences of pieces of at most 64 KiB and directories are
//! sorted name-to-inode maps, all kept in one inode table. Contents live
//! on the kernel heap and are lost at power-off.
//!
//! (A file is kept in pieces because the heap cannot always give one
//! large stretch of memory: the largest single allocation is a few
//! megabytes, and after a while not even that is certain. In pieces, a
//! file can be as large as there is memory.)

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::path::validate_name;
use crate::{DirEntry, FileSystem, FileType, FsError, FsResult, InodeId, Metadata};

const ROOT: InodeId = 1;

/// The size of every piece of a file but the last.
const PIECE: usize = 64 * 1024;
/// The largest file: beyond this, growing is refused outright rather
/// than tried until memory runs out.
pub const FILE_MAX: usize = 1 << 30;

/// The bytes of a file: full pieces, then one that may be shorter.
#[derive(Debug, Default)]
struct Contents {
    pieces: Vec<Vec<u8>>,
}

impl Contents {
    fn len(&self) -> usize {
        match self.pieces.split_last() {
            Some((last, full)) => full.len() * PIECE + last.len(),
            None => 0,
        }
    }

    /// Makes the file `len` bytes long: cut off, or continued with
    /// zeros. If memory runs out while growing, the file is left as it
    /// was.
    fn resize(&mut self, len: usize) -> FsResult<()> {
        let before = self.len();
        if len > FILE_MAX {
            return Err(FsError::NoSpace);
        }
        if len <= before {
            self.pieces.truncate(len.div_ceil(PIECE));
            if let Some(last) = self.pieces.last_mut() {
                last.truncate(len - (len - 1) / PIECE * PIECE);
            }
            return Ok(());
        }
        let grown = self.grow(len);
        if grown.is_err() {
            // Back to what it was; shrinking cannot fail.
            let _ = self.resize(before);
        }
        grown
    }

    fn grow(&mut self, len: usize) -> FsResult<()> {
        let mut missing = len - self.len();
        while missing != 0 {
            let room = match self.pieces.last() {
                Some(last) if last.len() < PIECE => PIECE - last.len(),
                _ => {
                    self.pieces.try_reserve(1).map_err(|_| FsError::NoSpace)?;
                    self.pieces.push(Vec::new());
                    PIECE
                }
            };
            let Some(last) = self.pieces.last_mut() else {
                return Err(FsError::NoSpace);
            };
            let more = room.min(missing);
            last.try_reserve(more).map_err(|_| FsError::NoSpace)?;
            last.resize(last.len() + more, 0);
            missing -= more;
        }
        Ok(())
    }

    /// Copies from `start` on into `buffer`; returns how many bytes
    /// there were.
    fn read(&self, start: usize, buffer: &mut [u8]) -> usize {
        let count = self.len().saturating_sub(start).min(buffer.len());
        let mut done = 0;
        while done < count {
            let at = start + done;
            let piece = &self.pieces[at / PIECE][at % PIECE..];
            let part = piece.len().min(count - done);
            buffer[done..done + part].copy_from_slice(&piece[..part]);
            done += part;
        }
        count
    }

    /// Puts `bytes` at `start`, making the file longer if they reach
    /// beyond its end (a gap before them is filled with zeros).
    fn write(&mut self, start: usize, bytes: &[u8]) -> FsResult<()> {
        let end = start.checked_add(bytes.len()).ok_or(FsError::NoSpace)?;
        if end > self.len() {
            self.resize(end)?;
        }
        let mut done = 0;
        while done < bytes.len() {
            let at = start + done;
            let piece = &mut self.pieces[at / PIECE][at % PIECE..];
            let part = piece.len().min(bytes.len() - done);
            piece[..part].copy_from_slice(&bytes[done..done + part]);
            done += part;
        }
        Ok(())
    }
}

#[derive(Debug)]
enum Node {
    File(Contents),
    Directory {
        /// The containing directory; the root is its own parent.
        parent: InodeId,
        entries: BTreeMap<String, InodeId>,
    },
}

impl Node {
    fn file_type(&self) -> FileType {
        match self {
            Self::File(_) => FileType::File,
            Self::Directory { .. } => FileType::Directory,
        }
    }
}

/// A RAM-backed [`FileSystem`].
#[derive(Debug)]
pub struct RamFs {
    nodes: BTreeMap<InodeId, Node>,
    next_inode: InodeId,
}

impl Default for RamFs {
    fn default() -> Self {
        Self::new()
    }
}

impl RamFs {
    /// Creates a filesystem containing only an empty root directory.
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT,
            Node::Directory {
                parent: ROOT,
                entries: BTreeMap::new(),
            },
        );
        Self {
            nodes,
            next_inode: ROOT + 1,
        }
    }

    /// Number of inodes, including the root.
    pub fn inode_count(&self) -> usize {
        self.nodes.len()
    }

    fn node(&self, inode: InodeId) -> FsResult<&Node> {
        self.nodes.get(&inode).ok_or(FsError::NotFound)
    }

    fn entries(&self, dir: InodeId) -> FsResult<&BTreeMap<String, InodeId>> {
        match self.node(dir)? {
            Node::Directory { entries, .. } => Ok(entries),
            Node::File(_) => Err(FsError::NotADirectory),
        }
    }

    fn entries_mut(&mut self, dir: InodeId) -> FsResult<&mut BTreeMap<String, InodeId>> {
        match self.nodes.get_mut(&dir).ok_or(FsError::NotFound)? {
            Node::Directory { entries, .. } => Ok(entries),
            Node::File(_) => Err(FsError::NotADirectory),
        }
    }

    fn file(&self, inode: InodeId) -> FsResult<&Contents> {
        match self.node(inode)? {
            Node::File(data) => Ok(data),
            Node::Directory { .. } => Err(FsError::IsADirectory),
        }
    }

    fn file_mut(&mut self, inode: InodeId) -> FsResult<&mut Contents> {
        match self.nodes.get_mut(&inode).ok_or(FsError::NotFound)? {
            Node::File(data) => Ok(data),
            Node::Directory { .. } => Err(FsError::IsADirectory),
        }
    }

    /// Whether `ancestor` is `inode` or one of its ancestors.
    fn is_same_or_ancestor(&self, ancestor: InodeId, mut inode: InodeId) -> bool {
        loop {
            if inode == ancestor {
                return true;
            }
            match self.nodes.get(&inode) {
                Some(Node::Directory { parent, .. }) if *parent != inode => inode = *parent,
                _ => return false,
            }
        }
    }
}

impl FileSystem for RamFs {
    fn root(&self) -> InodeId {
        ROOT
    }

    fn metadata(&self, inode: InodeId) -> FsResult<Metadata> {
        let node = self.node(inode)?;
        Ok(Metadata {
            inode,
            file_type: node.file_type(),
            size: match node {
                Node::File(data) => data.len() as u64,
                Node::Directory { entries, .. } => entries.len() as u64,
            },
        })
    }

    fn lookup(&self, dir: InodeId, name: &str) -> FsResult<InodeId> {
        self.entries(dir)?
            .get(name)
            .copied()
            .ok_or(FsError::NotFound)
    }

    fn read_dir(&self, dir: InodeId) -> FsResult<Vec<DirEntry>> {
        let entries = self.entries(dir)?;
        let mut list = Vec::new();
        list.try_reserve_exact(entries.len())
            .map_err(|_| FsError::NoSpace)?;
        for (name, &inode) in entries {
            list.push(DirEntry {
                name: name.clone(),
                inode,
                file_type: self.node(inode)?.file_type(),
            });
        }
        Ok(list)
    }

    fn create(&mut self, dir: InodeId, name: &str, file_type: FileType) -> FsResult<InodeId> {
        validate_name(name)?;
        if self.entries(dir)?.contains_key(name) {
            return Err(FsError::AlreadyExists);
        }
        let inode = self.next_inode;
        self.next_inode += 1;
        let node = match file_type {
            FileType::File => Node::File(Contents::default()),
            FileType::Directory => Node::Directory {
                parent: dir,
                entries: BTreeMap::new(),
            },
        };
        self.nodes.insert(inode, node);
        self.entries_mut(dir)?.insert(name.into(), inode);
        Ok(inode)
    }

    fn remove(&mut self, dir: InodeId, name: &str) -> FsResult<()> {
        let inode = self.lookup(dir, name)?;
        if let Node::Directory { entries, .. } = self.node(inode)?
            && !entries.is_empty()
        {
            return Err(FsError::DirectoryNotEmpty);
        }
        self.entries_mut(dir)?.remove(name);
        self.nodes.remove(&inode);
        Ok(())
    }

    fn rename(
        &mut self,
        from_dir: InodeId,
        from_name: &str,
        to_dir: InodeId,
        to_name: &str,
    ) -> FsResult<()> {
        validate_name(to_name)?;
        let inode = self.lookup(from_dir, from_name)?;
        if self.entries(to_dir)?.contains_key(to_name) {
            return Err(FsError::AlreadyExists);
        }
        // A directory must not become its own descendant.
        if self.is_same_or_ancestor(inode, to_dir) {
            return Err(FsError::InvalidArgument);
        }
        self.entries_mut(from_dir)?.remove(from_name);
        self.entries_mut(to_dir)?.insert(to_name.into(), inode);
        if let Some(Node::Directory { parent, .. }) = self.nodes.get_mut(&inode) {
            *parent = to_dir;
        }
        Ok(())
    }

    fn read(&self, inode: InodeId, offset: u64, buffer: &mut [u8]) -> FsResult<usize> {
        let data = self.file(inode)?;
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        Ok(data.read(start, buffer))
    }

    fn write(&mut self, inode: InodeId, offset: u64, bytes: &[u8]) -> FsResult<usize> {
        let data = self.file_mut(inode)?;
        let start = usize::try_from(offset).map_err(|_| FsError::NoSpace)?;
        data.write(start, bytes)?;
        Ok(bytes.len())
    }

    fn truncate(&mut self, inode: InodeId, size: u64) -> FsResult<()> {
        let data = self.file_mut(inode)?;
        let size = usize::try_from(size).map_err(|_| FsError::NoSpace)?;
        data.resize(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file is kept in pieces; nothing about that may show. Random
    /// writes, reads and changes of length are done to a file and to a
    /// plain vector, which must stay equal.
    #[test]
    fn a_file_in_pieces_behaves_like_one_stretch_of_bytes() {
        let mut fs = RamFs::new();
        let file = fs.create(fs.root(), "f", FileType::File).unwrap();
        let mut model: Vec<u8> = Vec::new();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |limit: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % limit as u64) as usize
        };
        // Offsets and lengths cluster around the boundaries of pieces.
        let place =
            |next: &mut dyn FnMut(usize) -> usize| next(5) * PIECE + next(7) * PIECE / 6 + next(3);
        for step in 0..600 {
            match next(4) {
                0 | 1 => {
                    let start = place(&mut next);
                    let bytes: Vec<u8> = (0..next(3 * PIECE / 2))
                        .map(|index| (index + step) as u8)
                        .collect();
                    assert_eq!(fs.write(file, start as u64, &bytes), Ok(bytes.len()));
                    if model.len() < start + bytes.len() {
                        model.resize(start + bytes.len(), 0);
                    }
                    model[start..start + bytes.len()].copy_from_slice(&bytes);
                }
                2 => {
                    let size = place(&mut next);
                    fs.truncate(file, size as u64).unwrap();
                    model.resize(size, 0);
                }
                _ => {
                    let start = place(&mut next);
                    let mut buffer = alloc::vec![0xEEu8; next(2 * PIECE)];
                    let count = fs.read(file, start as u64, &mut buffer).unwrap();
                    let expected = model.get(start..).unwrap_or(&[]);
                    let expected = &expected[..expected.len().min(buffer.len())];
                    assert_eq!(count, expected.len(), "step {step}");
                    assert!(&buffer[..count] == expected, "step {step}");
                }
            }
            assert_eq!(
                fs.metadata(file).unwrap().size,
                model.len() as u64,
                "step {step}"
            );
        }
        // The whole of it, in the end.
        let mut all = alloc::vec![0u8; model.len() + 10];
        let count = fs.read(file, 0, &mut all).unwrap();
        assert!(all[..count] == model[..]);
    }

    #[test]
    fn a_file_can_be_larger_than_any_one_allocation_and_has_a_limit() {
        let mut fs = RamFs::new();
        let file = fs.create(fs.root(), "big", FileType::File).unwrap();
        // 9 MiB, written the way a download arrives: in small parts.
        let part: Vec<u8> = (0..4096).map(|index| (index % 251) as u8).collect();
        let parts = 9 * 256;
        for index in 0..parts {
            fs.write(file, (index * part.len()) as u64, &part).unwrap();
        }
        assert_eq!(fs.metadata(file).unwrap().size, 9 << 20);
        // No piece of it is larger than 64 KiB.
        let Node::File(contents) = fs.node(file).unwrap() else {
            panic!("not a file");
        };
        assert!(contents.pieces.iter().all(|piece| piece.len() == PIECE));
        assert_eq!(contents.pieces.len(), 9 * 16);
        let mut back = alloc::vec![0u8; part.len()];
        for index in [0, 1, 255, 256, parts - 1] {
            assert_eq!(
                fs.read(file, (index * part.len()) as u64, &mut back),
                Ok(part.len())
            );
            assert!(back == part);
        }
        // Past the limit nothing is even tried, and the file is as it
        // was.
        assert_eq!(
            fs.truncate(file, FILE_MAX as u64 + 1),
            Err(FsError::NoSpace)
        );
        assert_eq!(fs.write(file, FILE_MAX as u64, b"x"), Err(FsError::NoSpace));
        assert_eq!(fs.metadata(file).unwrap().size, 9 << 20);
        // Cut to nothing, the memory is given back.
        fs.truncate(file, 0).unwrap();
        let Node::File(contents) = fs.node(file).unwrap() else {
            panic!("not a file");
        };
        assert!(contents.pieces.is_empty());
    }

    #[test]
    fn create_lookup_remove() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let dir = fs.create(root, "dir", FileType::Directory).unwrap();
        let file = fs.create(dir, "file", FileType::File).unwrap();
        assert_ne!(dir, file);
        assert_eq!(fs.lookup(root, "dir"), Ok(dir));
        assert_eq!(fs.lookup(dir, "file"), Ok(file));
        assert_eq!(fs.lookup(file, "x"), Err(FsError::NotADirectory));
        assert_eq!(fs.lookup(root, "nope"), Err(FsError::NotFound));
        assert_eq!(
            fs.create(root, "dir", FileType::File),
            Err(FsError::AlreadyExists)
        );
        assert_eq!(
            fs.create(root, "a/b", FileType::File),
            Err(FsError::InvalidName)
        );
        assert_eq!(
            fs.create(root, "..", FileType::File),
            Err(FsError::InvalidName)
        );
        assert_eq!(fs.inode_count(), 3);

        assert_eq!(fs.remove(root, "dir"), Err(FsError::DirectoryNotEmpty));
        fs.remove(dir, "file").unwrap();
        fs.remove(root, "dir").unwrap();
        assert_eq!(fs.inode_count(), 1);
        assert_eq!(fs.metadata(file), Err(FsError::NotFound));
    }

    #[test]
    fn read_write_truncate() {
        let mut fs = RamFs::new();
        let f = fs.create(fs.root(), "f", FileType::File).unwrap();
        assert_eq!(fs.write(f, 0, b"hello"), Ok(5));
        assert_eq!(fs.write(f, 8, b"!!"), Ok(2), "sparse write zero-fills");
        let mut buf = [0xFFu8; 16];
        assert_eq!(fs.read(f, 0, &mut buf), Ok(10));
        assert_eq!(&buf[..10], b"hello\0\0\0!!");
        assert_eq!(fs.read(f, 3, &mut buf[..4]), Ok(4));
        assert_eq!(&buf[..4], b"lo\0\0");
        assert_eq!(fs.read(f, 10, &mut buf), Ok(0));
        assert_eq!(fs.read(f, u64::MAX, &mut buf), Ok(0));

        fs.truncate(f, 2).unwrap();
        assert_eq!(fs.metadata(f).unwrap().size, 2);
        fs.truncate(f, 4).unwrap();
        assert_eq!(fs.read(f, 0, &mut buf), Ok(4));
        assert_eq!(&buf[..4], b"he\0\0");

        let root = fs.root();
        assert_eq!(fs.write(root, 0, b"x"), Err(FsError::IsADirectory));
        assert_eq!(fs.read(root, 0, &mut buf), Err(FsError::IsADirectory));
        assert_eq!(fs.write(f, u64::MAX, b"x"), Err(FsError::NoSpace));
    }

    #[test]
    fn listing_is_sorted_and_typed() {
        let mut fs = RamFs::new();
        let root = fs.root();
        fs.create(root, "zeta", FileType::File).unwrap();
        fs.create(root, "alpha", FileType::Directory).unwrap();
        fs.create(root, "mid", FileType::File).unwrap();
        let list = fs.read_dir(root).unwrap();
        let summary: Vec<_> = list
            .iter()
            .map(|e| (e.name.as_str(), e.file_type))
            .collect();
        assert_eq!(
            summary,
            [
                ("alpha", FileType::Directory),
                ("mid", FileType::File),
                ("zeta", FileType::File)
            ]
        );
        assert_eq!(fs.metadata(root).unwrap().size, 3);
    }

    #[test]
    fn rename_updates_parent_and_blocks_cycles() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let a = fs.create(root, "a", FileType::Directory).unwrap();
        let b = fs.create(a, "b", FileType::Directory).unwrap();
        let c = fs.create(b, "c", FileType::Directory).unwrap();

        assert_eq!(fs.rename(root, "a", c, "a"), Err(FsError::InvalidArgument));
        assert_eq!(
            fs.rename(root, "a", a, "self"),
            Err(FsError::InvalidArgument)
        );

        // Move /a/b to /b, then /a into /b/c: legal only because the parent
        // link of `b` was updated by the first move.
        fs.rename(a, "b", root, "b").unwrap();
        fs.rename(root, "a", c, "a").unwrap();
        assert_eq!(fs.lookup(c, "a"), Ok(a));
        assert_eq!(fs.rename(root, "b", a, "b"), Err(FsError::InvalidArgument));
    }
}
