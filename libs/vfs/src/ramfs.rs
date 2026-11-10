// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! In-memory filesystem.
//!
//! Files are byte vectors and directories are sorted name-to-inode maps,
//! all kept in one inode table. Contents live on the kernel heap and are
//! lost at power-off.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::path::validate_name;
use crate::{DirEntry, FileSystem, FileType, FsError, FsResult, InodeId, Metadata};

const ROOT: InodeId = 1;

#[derive(Debug)]
enum Node {
    File(Vec<u8>),
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

    fn file(&self, inode: InodeId) -> FsResult<&Vec<u8>> {
        match self.node(inode)? {
            Node::File(data) => Ok(data),
            Node::Directory { .. } => Err(FsError::IsADirectory),
        }
    }

    fn file_mut(&mut self, inode: InodeId) -> FsResult<&mut Vec<u8>> {
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

/// Grows `data` to `len` bytes, zero-filled, without aborting on allocation
/// failure.
fn grow(data: &mut Vec<u8>, len: usize) -> FsResult<()> {
    if len > data.len() {
        data.try_reserve(len - data.len())
            .map_err(|_| FsError::NoSpace)?;
        data.resize(len, 0);
    }
    Ok(())
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
            FileType::File => Node::File(Vec::new()),
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
        let available = data.get(start..).unwrap_or(&[]);
        let count = available.len().min(buffer.len());
        buffer[..count].copy_from_slice(&available[..count]);
        Ok(count)
    }

    fn write(&mut self, inode: InodeId, offset: u64, bytes: &[u8]) -> FsResult<usize> {
        let data = self.file_mut(inode)?;
        let start = usize::try_from(offset).map_err(|_| FsError::NoSpace)?;
        let end = start.checked_add(bytes.len()).ok_or(FsError::NoSpace)?;
        grow(data, end)?;
        data[start..end].copy_from_slice(bytes);
        Ok(bytes.len())
    }

    fn truncate(&mut self, inode: InodeId, size: u64) -> FsResult<()> {
        let data = self.file_mut(inode)?;
        let size = usize::try_from(size).map_err(|_| FsError::NoSpace)?;
        if size <= data.len() {
            data.truncate(size);
            Ok(())
        } else {
            grow(data, size)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
