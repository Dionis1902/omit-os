// SPDX-FileCopyrightText: 2026 Omit OS contributors
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
pub mod tar;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
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
    /// The storage device failed, or the on-disk data is corrupt.
    Io,
    /// The object is in use (for example, it is a mount point).
    Busy,
    /// The operation would move something between filesystems.
    CrossDevice,
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
            Self::Io => "Input/output error",
            Self::Busy => "Device or resource busy",
            Self::CrossDevice => "Invalid cross-device link",
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

/// One mounted filesystem.
struct Mount {
    /// Normalised absolute path of the mount point; `/` for the root.
    path: String,
    filesystem: Box<dyn FileSystem>,
}

/// Path-based access to the mounted filesystems.
///
/// Every `path` argument must be absolute and normalised; use
/// [`path::normalize`] to turn user input into that form.
///
/// One filesystem is the root; others can be mounted on existing
/// directories. A path belongs to the filesystem with the longest mount
/// point that is a prefix of it.
pub struct Vfs {
    /// The root filesystem is always first.
    mounts: Vec<Mount>,
}

/// A location inside one mounted filesystem.
#[derive(Clone, Copy)]
struct Location {
    mount: usize,
    inode: InodeId,
}

impl Vfs {
    /// Creates a VFS with `root` mounted at `/`.
    pub fn new(root: Box<dyn FileSystem>) -> Self {
        Self {
            mounts: vec![Mount {
                path: "/".into(),
                filesystem: root,
            }],
        }
    }

    /// Mounts `filesystem` on the existing directory `path`. The directory's
    /// own contents are hidden while the mount is in place.
    pub fn mount(&mut self, path: &str, filesystem: Box<dyn FileSystem>) -> FsResult<()> {
        if self.metadata(path)?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        if self.mounts.iter().any(|mount| mount.path == path) {
            return Err(FsError::Busy);
        }
        self.mounts.push(Mount {
            path: path.into(),
            filesystem,
        });
        Ok(())
    }

    /// Unmounts the filesystem mounted at `path` and returns it. Fails with
    /// [`FsError::Busy`] if another filesystem is mounted below it, and
    /// with [`FsError::InvalidArgument`] for the root or a path that is not
    /// a mount point.
    pub fn unmount(&mut self, path: &str) -> FsResult<Box<dyn FileSystem>> {
        let index = self
            .mounts
            .iter()
            .position(|mount| mount.path == path)
            .filter(|&index| index != 0)
            .ok_or(FsError::InvalidArgument)?;
        if self
            .mounts
            .iter()
            .any(|mount| path::is_inside(&mount.path, path))
        {
            return Err(FsError::Busy);
        }
        Ok(self.mounts.remove(index).filesystem)
    }

    /// The mount points, root first.
    pub fn mount_points(&self) -> Vec<String> {
        self.mounts.iter().map(|mount| mount.path.clone()).collect()
    }

    fn is_mount_point(&self, path: &str) -> bool {
        self.mounts.iter().any(|mount| mount.path == path)
    }

    /// Finds the filesystem `path` belongs to and the path within it.
    fn locate<'p>(&self, path: &'p str) -> FsResult<(usize, &'p str)> {
        // Validates the path as a side effect.
        let _ = path::components(path)?;
        let (index, mount) = self
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, mount)| mount.path == path || path::is_inside(path, &mount.path))
            .max_by_key(|(_, mount)| mount.path.len())
            .ok_or(FsError::NotFound)?;
        let inner = if mount.path == "/" {
            path
        } else if path.len() == mount.path.len() {
            "/"
        } else {
            &path[mount.path.len()..]
        };
        Ok((index, inner))
    }

    fn fs(&self, mount: usize) -> &dyn FileSystem {
        self.mounts[mount].filesystem.as_ref()
    }

    fn fs_mut(&mut self, mount: usize) -> &mut dyn FileSystem {
        self.mounts[mount].filesystem.as_mut()
    }

    /// Walks `path` and returns where it leads.
    fn resolve(&self, path: &str) -> FsResult<Location> {
        let (mount, inner) = self.locate(path)?;
        let filesystem = self.fs(mount);
        let mut inode = filesystem.root();
        for component in path::components(inner)? {
            if filesystem.metadata(inode)?.file_type != FileType::Directory {
                return Err(FsError::NotADirectory);
            }
            inode = filesystem.lookup(inode, component)?;
        }
        Ok(Location { mount, inode })
    }

    /// Resolves the parent directory of `path` and returns it with the final
    /// component. Fails for `/` and for mount points, which have no entry
    /// in a parent directory of their own filesystem.
    fn resolve_parent<'p>(&self, path: &'p str) -> FsResult<(Location, &'p str)> {
        let (parent, name) = path::split(path)?.ok_or(FsError::InvalidArgument)?;
        if self.is_mount_point(path) {
            return Err(FsError::Busy);
        }
        let location = self.resolve(parent)?;
        if self.fs(location.mount).metadata(location.inode)?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        Ok((location, name))
    }

    /// Describes the file or directory at `path`.
    pub fn metadata(&self, path: &str) -> FsResult<Metadata> {
        let location = self.resolve(path)?;
        self.fs(location.mount).metadata(location.inode)
    }

    /// Lists the directory at `path`, sorted by name.
    pub fn read_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let location = self.resolve(path)?;
        let filesystem = self.fs(location.mount);
        if filesystem.metadata(location.inode)?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        filesystem.read_dir(location.inode)
    }

    /// Creates the directory `path`. Its parent must exist.
    pub fn create_dir(&mut self, path: &str) -> FsResult<()> {
        if path == "/" || self.is_mount_point(path) {
            return Err(FsError::AlreadyExists);
        }
        let (parent, name) = self.resolve_parent(path)?;
        self.fs_mut(parent.mount)
            .create(parent.inode, name, FileType::Directory)
            .map(|_| ())
    }

    /// Creates the directory `path` and any missing ancestors. Succeeds if
    /// it already exists as a directory.
    pub fn create_dir_all(&mut self, path: &str) -> FsResult<()> {
        // Validate first, then create one prefix at a time so that mount
        // points along the way are honoured.
        let _ = path::components(path)?;
        let mut end = 0;
        while end < path.len() {
            end = path[end + 1..]
                .find('/')
                .map_or(path.len(), |offset| end + 1 + offset);
            let prefix = &path[..end];
            match self.metadata(prefix) {
                Ok(metadata) if metadata.file_type == FileType::Directory => {}
                Ok(_) => return Err(FsError::NotADirectory),
                Err(FsError::NotFound) => self.create_dir(prefix)?,
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    /// Reads the whole file at `path`.
    pub fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let location = self.resolve(path)?;
        let filesystem = self.fs(location.mount);
        let metadata = filesystem.metadata(location.inode)?;
        if metadata.file_type != FileType::File {
            return Err(FsError::IsADirectory);
        }
        let size = usize::try_from(metadata.size).map_err(|_| FsError::NoSpace)?;
        let mut data = Vec::new();
        data.try_reserve_exact(size).map_err(|_| FsError::NoSpace)?;
        data.resize(size, 0);
        let read = filesystem.read(location.inode, 0, &mut data)?;
        data.truncate(read);
        Ok(data)
    }

    /// Writes `data` to the file at `path`, creating it if necessary.
    pub fn write_file(&mut self, path: &str, data: &[u8], mode: WriteMode) -> FsResult<()> {
        if path == "/" || self.is_mount_point(path) {
            return Err(FsError::IsADirectory);
        }
        let (parent, name) = self.resolve_parent(path)?;
        let filesystem = self.fs_mut(parent.mount);
        let inode = match filesystem.lookup(parent.inode, name) {
            Ok(inode) => inode,
            Err(FsError::NotFound) => filesystem.create(parent.inode, name, FileType::File)?,
            Err(other) => return Err(other),
        };
        let metadata = filesystem.metadata(inode)?;
        if metadata.file_type != FileType::File {
            return Err(FsError::IsADirectory);
        }
        let offset = match mode {
            WriteMode::Truncate => {
                filesystem.truncate(inode, 0)?;
                0
            }
            WriteMode::Append => metadata.size,
        };
        let written = filesystem.write(inode, offset, data)?;
        if written != data.len() {
            return Err(FsError::NoSpace);
        }
        Ok(())
    }

    /// Reads from the file at `path` starting at `offset` into `buffer`;
    /// returns the number of bytes read (0 at or past the end).
    pub fn read_at(&self, path: &str, offset: u64, buffer: &mut [u8]) -> FsResult<usize> {
        let location = self.resolve(path)?;
        let filesystem = self.fs(location.mount);
        if filesystem.metadata(location.inode)?.file_type != FileType::File {
            return Err(FsError::IsADirectory);
        }
        filesystem.read(location.inode, offset, buffer)
    }

    /// Writes `data` to the existing file at `path` starting at `offset`
    /// (or at its end, with `offset` of `None`); returns the offset just
    /// past the written data.
    pub fn write_at(&mut self, path: &str, offset: Option<u64>, data: &[u8]) -> FsResult<u64> {
        let location = self.resolve(path)?;
        let filesystem = self.fs_mut(location.mount);
        let metadata = filesystem.metadata(location.inode)?;
        if metadata.file_type != FileType::File {
            return Err(FsError::IsADirectory);
        }
        let offset = offset.unwrap_or(metadata.size);
        let written = filesystem.write(location.inode, offset, data)?;
        if written != data.len() {
            return Err(FsError::NoSpace);
        }
        Ok(offset + written as u64)
    }

    /// Removes the file or empty directory at `path`. Mount points cannot
    /// be removed.
    pub fn remove(&mut self, path: &str) -> FsResult<()> {
        let (parent, name) = self.resolve_parent(path)?;
        self.fs_mut(parent.mount).remove(parent.inode, name)
    }

    /// Removes `path` and, if it is a directory, everything below it.
    /// Fails with [`FsError::Busy`] if a filesystem is mounted at or below
    /// `path`.
    pub fn remove_all(&mut self, path: &str) -> FsResult<()> {
        if self
            .mounts
            .iter()
            .any(|mount| path::is_inside(&mount.path, path))
        {
            return Err(FsError::Busy);
        }
        let (parent, name) = self.resolve_parent(path)?;
        let inode = self.fs(parent.mount).lookup(parent.inode, name)?;
        self.remove_tree(parent.mount, parent.inode, name, inode)
    }

    /// Depth-first removal within one filesystem. Recursion depth equals
    /// directory depth, which path-length limits keep small.
    fn remove_tree(
        &mut self,
        mount: usize,
        dir: InodeId,
        name: &str,
        inode: InodeId,
    ) -> FsResult<()> {
        if self.fs(mount).metadata(inode)?.file_type == FileType::Directory {
            for entry in self.fs(mount).read_dir(inode)? {
                self.remove_tree(mount, inode, &entry.name, entry.inode)?;
            }
        }
        self.fs_mut(mount).remove(dir, name)
    }

    /// Moves or renames `from` to `to`. `to` must not exist, a directory
    /// cannot be moved into itself, and both must be on the same
    /// filesystem.
    pub fn rename(&mut self, from: &str, to: &str) -> FsResult<()> {
        let (from_parent, from_name) = self.resolve_parent(from)?;
        let (to_parent, to_name) = self.resolve_parent(to)?;
        if from_parent.mount != to_parent.mount {
            return Err(FsError::CrossDevice);
        }
        self.fs_mut(from_parent.mount).rename(
            from_parent.inode,
            from_name,
            to_parent.inode,
            to_name,
        )
    }

    /// Copies the regular file `from` to `to`, replacing `to` if it is an
    /// existing file. Works across filesystems.
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
    fn offset_io() {
        let mut v = vfs();
        v.write_file("/f", b"0123456789", WriteMode::Truncate)
            .unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(v.read_at("/f", 3, &mut buf), Ok(4));
        assert_eq!(&buf, b"3456");
        assert_eq!(v.read_at("/f", 8, &mut buf), Ok(2));
        assert_eq!(v.read_at("/f", 10, &mut buf), Ok(0));
        assert_eq!(v.write_at("/f", Some(2), b"AB"), Ok(4));
        assert_eq!(v.write_at("/f", None, b"!"), Ok(11));
        assert_eq!(v.read_file("/f").unwrap(), b"01AB456789!");
        assert_eq!(v.write_at("/nope", None, b"x"), Err(FsError::NotFound));
        v.create_dir("/d").unwrap();
        assert_eq!(v.read_at("/d", 0, &mut buf), Err(FsError::IsADirectory));
        assert_eq!(v.write_at("/d", None, b"x"), Err(FsError::IsADirectory));
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
    fn mounts() {
        let mut v = vfs();
        v.create_dir_all("/mnt/disk").unwrap();
        v.write_file("/mnt/disk/hidden", b"under", WriteMode::Truncate)
            .unwrap();
        v.write_file("/file", b"x", WriteMode::Truncate).unwrap();

        assert_eq!(
            v.mount("/nope", Box::new(RamFs::new())).err(),
            Some(FsError::NotFound)
        );
        assert_eq!(
            v.mount("/file", Box::new(RamFs::new())).err(),
            Some(FsError::NotADirectory)
        );
        v.mount("/mnt/disk", Box::new(RamFs::new())).unwrap();
        assert_eq!(
            v.mount("/mnt/disk", Box::new(RamFs::new())).err(),
            Some(FsError::Busy)
        );
        assert_eq!(v.mount_points(), ["/", "/mnt/disk"]);

        // The mounted filesystem is empty and separate from the root.
        assert!(v.read_dir("/mnt/disk").unwrap().is_empty());
        v.create_dir_all("/mnt/disk/a/b").unwrap();
        v.write_file("/mnt/disk/a/f", b"on disk", WriteMode::Truncate)
            .unwrap();
        assert_eq!(v.read_file("/mnt/disk/a/f").unwrap(), b"on disk");
        assert_eq!(names(&v, "/mnt"), ["disk"]);
        assert_eq!(names(&v, "/mnt/disk"), ["a"]);
        assert_eq!(
            v.metadata("/mnt/disk").unwrap().file_type,
            FileType::Directory
        );

        // Mount points are pinned, and renames do not cross filesystems.
        assert_eq!(v.remove("/mnt/disk"), Err(FsError::Busy));
        assert_eq!(v.remove_all("/mnt"), Err(FsError::Busy));
        assert_eq!(v.rename("/mnt/disk", "/elsewhere"), Err(FsError::Busy));
        assert_eq!(
            v.rename("/file", "/mnt/disk/file"),
            Err(FsError::CrossDevice)
        );
        v.copy_file("/file", "/mnt/disk/file").unwrap();
        assert_eq!(v.read_file("/mnt/disk/file").unwrap(), b"x");
        v.rename("/mnt/disk/file", "/mnt/disk/a/b/moved").unwrap();
        assert_eq!(v.create_dir("/mnt/disk"), Err(FsError::AlreadyExists));
        assert_eq!(
            v.write_file("/mnt/disk", b"", WriteMode::Truncate),
            Err(FsError::IsADirectory)
        );

        // A nested mount blocks unmounting its parent.
        v.mount("/mnt/disk/a", Box::new(RamFs::new())).unwrap();
        assert_eq!(v.unmount("/mnt/disk").err(), Some(FsError::Busy));
        assert!(v.read_dir("/mnt/disk/a").unwrap().is_empty());
        v.unmount("/mnt/disk/a").unwrap();
        assert_eq!(names(&v, "/mnt/disk/a"), ["b", "f"]);

        assert_eq!(v.unmount("/").err(), Some(FsError::InvalidArgument));
        assert_eq!(v.unmount("/mnt").err(), Some(FsError::InvalidArgument));
        let detached = v.unmount("/mnt/disk").unwrap();
        assert_eq!(
            names(&v, "/mnt/disk"),
            ["hidden"],
            "the directory reappears"
        );

        // The unmounted filesystem kept its contents.
        let other = Vfs::new(detached);
        assert_eq!(other.read_file("/a/b/moved").unwrap(), b"x");
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
