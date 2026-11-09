// SPDX-License-Identifier: GPL-2.0-only
//! Virtual filesystem layer.
//!
//! * [`FileSystem`] is the interface a concrete filesystem implements. It
//!   speaks in inode numbers and single path components.
//! * [`Vfs`] sits on top and offers path-based operations: it normalises
//!   paths, walks directories and enforces the rules that are the same for
//!   every filesystem.
//! * [`ramfs::RamFs`] is an in-memory filesystem.
//!
//! The crate is `no_std` + `alloc`, contains no `unsafe` code and is
//! unit-tested on the host.
//!
//! # Current scope
//!
//! One filesystem mounted at `/`; no mount points, permissions, timestamps,
//! links or open-file handles yet. Those arrive with the features that need
//! them (persistent storage, processes with file descriptors).

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod path;
pub mod ramfs;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// Identifies a file or directory within one filesystem.
pub type InodeId = u64;

/// Longest permitted file name, in bytes.
pub const NAME_MAX: usize = 255;

/// What an inode is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// A regular file.
    File,
    /// A directory.
    Directory,
}

/// Information about an inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    /// The inode number.
    pub inode: InodeId,
    /// File or directory.
    pub file_type: FileType,
    /// Size in bytes for files; number of entries for directories.
    pub size: u64,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's name within its directory.
    pub name: String,
    /// The inode it refers to.
    pub inode: InodeId,
    /// File or directory.
    pub file_type: FileType,
}

/// Filesystem errors. The `Display` text follows the conventional POSIX
/// wording so shell output looks familiar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// The path, or a component of it, does not exist.
    NotFound,
    /// A path component that must be a directory is not one.
    NotADirectory,
    /// The operation needs a regular file but found a directory.
    IsADirectory,
    /// The name is already taken.
    AlreadyExists,
    /// The directory still has entries.
    DirectoryNotEmpty,
    /// The name is empty, too long, or contains `/` or NUL.
    InvalidName,
    /// The operation makes no sense for this path (for example removing
    /// `/`, or moving a directory into itself).
    InvalidArgument,
    /// Out of memory or storage.
    NoSpace,
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::NotFound => "No such file or directory",
            Self::NotADirectory => "Not a directory",
            Self::IsADirectory => "Is a directory",
            Self::AlreadyExists => "File exists",
            Self::DirectoryNotEmpty => "Directory not empty",
            Self::InvalidName => "Invalid file name",
            Self::InvalidArgument => "Invalid argument",
            Self::NoSpace => "No space left on device",
        })
    }
}

/// Result type of filesystem operations.
pub type FsResult<T> = Result<T, FsError>;

/// The operations a concrete filesystem provides.
///
/// Names passed to these methods are single, already validated path
/// components (never empty, `.`, `..`, or containing `/`).
pub trait FileSystem: Send {
    /// The inode of the root directory.
    fn root(&self) -> InodeId;

    /// Describes `inode`.
    fn metadata(&self, inode: InodeId) -> FsResult<Metadata>;

    /// Finds `name` in directory `dir`.
    fn lookup(&self, dir: InodeId, name: &str) -> FsResult<InodeId>;

    /// Lists directory `dir`, sorted by name.
    fn read_dir(&self, dir: InodeId) -> FsResult<Vec<DirEntry>>;

    /// Creates an empty file or directory called `name` in `dir`.
    fn create(&mut self, dir: InodeId, name: &str, file_type: FileType) -> FsResult<InodeId>;

    /// Removes `name` from `dir`. Directories must be empty.
    fn remove(&mut self, dir: InodeId, name: &str) -> FsResult<()>;

    /// Moves `from_name` in `from_dir` to `to_name` in `to_dir`, which must
    /// not exist.
    fn rename(
        &mut self,
        from_dir: InodeId,
        from_name: &str,
        to_dir: InodeId,
        to_name: &str,
    ) -> FsResult<()>;

    /// Reads from file `inode` at `offset` into `buffer`; returns the number
    /// of bytes read (0 at or past the end).
    fn read(&self, inode: InodeId, offset: u64, buffer: &mut [u8]) -> FsResult<usize>;

    /// Writes `data` to file `inode` at `offset`, extending it (zero-filled)
    /// as needed.
    fn write(&mut self, inode: InodeId, offset: u64, data: &[u8]) -> FsResult<usize>;

    /// Sets the size of file `inode`, discarding or zero-filling.
    fn truncate(&mut self, inode: InodeId, size: u64) -> FsResult<()>;
}

/// How [`Vfs::write_file`] treats existing contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Replace the file's contents.
    Truncate,
    /// Add to the end of the file.
    Append,
}

/// Path-based access to a mounted filesystem.
///
/// Every `path` argument must be absolute and normalised; use
/// [`path::normalize`] to turn user input into that form.
pub struct Vfs {
    root: Box<dyn FileSystem>,
}

impl Vfs {
    /// Creates a VFS with `root` mounted at `/`.
    pub fn new(root: Box<dyn FileSystem>) -> Self {
        Self { root }
    }

    /// Walks `path` from the root and returns the inode it names.
    fn resolve(&self, path: &str) -> FsResult<InodeId> {
        let mut inode = self.root.root();
        for component in path::components(path)? {
            if self.root.metadata(inode)?.file_type != FileType::Directory {
                return Err(FsError::NotADirectory);
            }
            inode = self.root.lookup(inode, component)?;
        }
        Ok(inode)
    }

    /// Resolves the parent directory of `path` and returns it with the final
    /// component. Fails for `/`, which has no parent.
    fn resolve_parent<'p>(&self, path: &'p str) -> FsResult<(InodeId, &'p str)> {
        let (parent, name) = path::split(path)?.ok_or(FsError::InvalidArgument)?;
        let dir = self.resolve(parent)?;
        if self.root.metadata(dir)?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        Ok((dir, name))
    }

    /// Describes the file or directory at `path`.
    pub fn metadata(&self, path: &str) -> FsResult<Metadata> {
        self.root.metadata(self.resolve(path)?)
    }

    /// Lists the directory at `path`, sorted by name.
    pub fn read_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let inode = self.resolve(path)?;
        if self.root.metadata(inode)?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        self.root.read_dir(inode)
    }

    /// Creates the directory `path`. Its parent must exist.
    pub fn create_dir(&mut self, path: &str) -> FsResult<()> {
        if path == "/" {
            return Err(FsError::AlreadyExists);
        }
        let (dir, name) = self.resolve_parent(path)?;
        self.root.create(dir, name, FileType::Directory).map(|_| ())
    }

    /// Creates the directory `path` and any missing ancestors. Succeeds if
    /// it already exists as a directory.
    pub fn create_dir_all(&mut self, path: &str) -> FsResult<()> {
        let mut inode = self.root.root();
        for component in path::components(path)? {
            inode = match self.root.lookup(inode, component) {
                Ok(existing) => {
                    if self.root.metadata(existing)?.file_type != FileType::Directory {
                        return Err(FsError::NotADirectory);
                    }
                    existing
                }
                Err(FsError::NotFound) => {
                    self.root.create(inode, component, FileType::Directory)?
                }
                Err(other) => return Err(other),
            };
        }
        Ok(())
    }

    /// Reads the whole file at `path`.
    pub fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let inode = self.resolve(path)?;
        let metadata = self.root.metadata(inode)?;
        if metadata.file_type != FileType::File {
            return Err(FsError::IsADirectory);
        }
        let size = usize::try_from(metadata.size).map_err(|_| FsError::NoSpace)?;
        let mut data = Vec::new();
        data.try_reserve_exact(size).map_err(|_| FsError::NoSpace)?;
        data.resize(size, 0);
        let read = self.root.read(inode, 0, &mut data)?;
        data.truncate(read);
        Ok(data)
    }

    /// Writes `data` to the file at `path`, creating it if necessary.
    pub fn write_file(&mut self, path: &str, data: &[u8], mode: WriteMode) -> FsResult<()> {
        if path == "/" {
            return Err(FsError::IsADirectory);
        }
        let (dir, name) = self.resolve_parent(path)?;
        let inode = match self.root.lookup(dir, name) {
            Ok(inode) => inode,
            Err(FsError::NotFound) => self.root.create(dir, name, FileType::File)?,
            Err(other) => return Err(other),
        };
        let metadata = self.root.metadata(inode)?;
        if metadata.file_type != FileType::File {
            return Err(FsError::IsADirectory);
        }
        let offset = match mode {
            WriteMode::Truncate => {
                self.root.truncate(inode, 0)?;
                0
            }
            WriteMode::Append => metadata.size,
        };
        let written = self.root.write(inode, offset, data)?;
        if written != data.len() {
            return Err(FsError::NoSpace);
        }
        Ok(())
    }

    /// Removes the file or empty directory at `path`.
    pub fn remove(&mut self, path: &str) -> FsResult<()> {
        let (dir, name) = self.resolve_parent(path)?;
        self.root.remove(dir, name)
    }

    /// Removes `path` and, if it is a directory, everything below it.
    pub fn remove_all(&mut self, path: &str) -> FsResult<()> {
        let (dir, name) = self.resolve_parent(path)?;
        let inode = self.root.lookup(dir, name)?;
        self.remove_tree(dir, name, inode)
    }

    /// Depth-first removal. Recursion depth equals directory depth, which
    /// path-length limits keep small.
    fn remove_tree(&mut self, dir: InodeId, name: &str, inode: InodeId) -> FsResult<()> {
        if self.root.metadata(inode)?.file_type == FileType::Directory {
            for entry in self.root.read_dir(inode)? {
                self.remove_tree(inode, &entry.name, entry.inode)?;
            }
        }
        self.root.remove(dir, name)
    }

    /// Moves or renames `from` to `to`. `to` must not exist, and a directory
    /// cannot be moved into itself.
    pub fn rename(&mut self, from: &str, to: &str) -> FsResult<()> {
        let (from_dir, from_name) = self.resolve_parent(from)?;
        let (to_dir, to_name) = self.resolve_parent(to)?;
        self.root.rename(from_dir, from_name, to_dir, to_name)
    }

    /// Copies the regular file `from` to `to`, replacing `to` if it is an
    /// existing file.
    pub fn copy_file(&mut self, from: &str, to: &str) -> FsResult<()> {
        let data = self.read_file(from)?;
        self.write_file(to, &data, WriteMode::Truncate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ramfs::RamFs;
    use alloc::string::ToString;

    fn vfs() -> Vfs {
        Vfs::new(Box::new(RamFs::new()))
    }

    fn names(vfs: &Vfs, path: &str) -> Vec<String> {
        vfs.read_dir(path)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect()
    }

    #[test]
    fn root_starts_empty() {
        let v = vfs();
        assert!(v.read_dir("/").unwrap().is_empty());
        assert_eq!(v.metadata("/").unwrap().file_type, FileType::Directory);
        assert_eq!(v.metadata("/nope"), Err(FsError::NotFound));
    }

    #[test]
    fn directories_and_files() {
        let mut v = vfs();
        v.create_dir("/etc").unwrap();
        v.create_dir("/etc/conf.d").unwrap();
        assert_eq!(v.create_dir("/etc"), Err(FsError::AlreadyExists));
        assert_eq!(v.create_dir("/"), Err(FsError::AlreadyExists));
        assert_eq!(v.create_dir("/a/b"), Err(FsError::NotFound));

        v.write_file("/etc/motd", b"hello\n", WriteMode::Truncate)
            .unwrap();
        v.write_file("/etc/motd", b"world\n", WriteMode::Append)
            .unwrap();
        assert_eq!(v.read_file("/etc/motd").unwrap(), b"hello\nworld\n");
        v.write_file("/etc/motd", b"x", WriteMode::Truncate)
            .unwrap();
        assert_eq!(v.read_file("/etc/motd").unwrap(), b"x");
        assert_eq!(v.metadata("/etc/motd").unwrap().size, 1);

        assert_eq!(names(&v, "/etc"), ["conf.d", "motd"]);
        assert_eq!(v.read_dir("/etc/motd"), Err(FsError::NotADirectory));
        assert_eq!(v.read_file("/etc"), Err(FsError::IsADirectory));
        assert_eq!(
            v.write_file("/etc", b"", WriteMode::Truncate),
            Err(FsError::IsADirectory)
        );
        assert_eq!(
            v.write_file("/", b"", WriteMode::Truncate),
            Err(FsError::IsADirectory)
        );
        assert_eq!(v.metadata("/etc/motd/x"), Err(FsError::NotADirectory));
        assert_eq!(
            v.write_file("/etc/motd/x", b"", WriteMode::Truncate),
            Err(FsError::NotADirectory)
        );
    }

    #[test]
    fn create_dir_all_is_idempotent() {
        let mut v = vfs();
        v.create_dir_all("/a/b/c").unwrap();
        v.create_dir_all("/a/b/c").unwrap();
        v.create_dir_all("/").unwrap();
        assert_eq!(names(&v, "/a/b"), ["c"]);
        v.write_file("/a/file", b"", WriteMode::Truncate).unwrap();
        assert_eq!(v.create_dir_all("/a/file/d"), Err(FsError::NotADirectory));
    }

    #[test]
    fn remove_and_remove_all() {
        let mut v = vfs();
        v.create_dir_all("/d/sub/deep").unwrap();
        v.write_file("/d/sub/f", b"1", WriteMode::Truncate).unwrap();
        assert_eq!(v.remove("/d"), Err(FsError::DirectoryNotEmpty));
        assert_eq!(v.remove("/missing"), Err(FsError::NotFound));
        assert_eq!(v.remove("/"), Err(FsError::InvalidArgument));
        assert_eq!(v.remove_all("/"), Err(FsError::InvalidArgument));
        v.remove("/d/sub/f").unwrap();
        v.remove_all("/d").unwrap();
        assert!(v.read_dir("/").unwrap().is_empty());
    }

    #[test]
    fn rename_rules() {
        let mut v = vfs();
        v.create_dir_all("/a/b").unwrap();
        v.write_file("/f", b"data", WriteMode::Truncate).unwrap();

        v.rename("/f", "/a/g").unwrap();
        assert_eq!(v.read_file("/a/g").unwrap(), b"data");
        assert_eq!(v.metadata("/f"), Err(FsError::NotFound));

        assert_eq!(v.rename("/a", "/a/b/c"), Err(FsError::InvalidArgument));
        assert_eq!(v.rename("/a/g", "/a/b"), Err(FsError::AlreadyExists));
        assert_eq!(v.rename("/nope", "/x"), Err(FsError::NotFound));
        assert_eq!(v.rename("/", "/x"), Err(FsError::InvalidArgument));

        v.rename("/a/b", "/b").unwrap();
        assert_eq!(names(&v, "/"), ["a", "b"]);
        v.rename("/a/g", "/a/g").unwrap_err();
    }

    #[test]
    fn copy_file() {
        let mut v = vfs();
        v.write_file("/src", b"contents", WriteMode::Truncate)
            .unwrap();
        v.copy_file("/src", "/dst").unwrap();
        v.write_file("/src", b"changed", WriteMode::Truncate)
            .unwrap();
        assert_eq!(v.read_file("/dst").unwrap(), b"contents");
        v.create_dir("/dir").unwrap();
        assert_eq!(v.copy_file("/dir", "/x"), Err(FsError::IsADirectory));
        assert_eq!(v.copy_file("/src", "/dir"), Err(FsError::IsADirectory));
    }

    #[test]
    fn names_are_validated() {
        let mut v = vfs();
        let long = "x".repeat(NAME_MAX + 1);
        let mut p = "/".to_string();
        p.push_str(&long);
        assert_eq!(v.create_dir(&p), Err(FsError::InvalidName));
        assert_eq!(v.create_dir("relative"), Err(FsError::InvalidArgument));
        assert_eq!(v.create_dir("/a//b"), Err(FsError::InvalidArgument));
        assert_eq!(v.create_dir("/a/../b"), Err(FsError::InvalidArgument));
    }
}
