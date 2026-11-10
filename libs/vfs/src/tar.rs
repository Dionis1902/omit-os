// SPDX-License-Identifier: GPL-2.0-only
//! Reading `ustar` archives.
//!
//! The initial ramdisk handed over by the boot loader is a plain POSIX tar
//! archive: a well-known, trivially simple format that standard tools can
//! create and inspect. Only regular files and directories are supported;
//! other entry types are skipped.

use alloc::string::String;

use crate::path::normalize;
use crate::{FsError, Vfs, WriteMode};

const BLOCK: usize = 512;

/// A problem with the archive itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarError {
    /// The archive ends in the middle of a header or of file data.
    Truncated,
    /// A header's checksum does not match its contents.
    BadChecksum,
    /// A numeric field is not valid octal, or a name is not valid UTF-8.
    BadField,
    /// Writing an entry to the filesystem failed.
    Fs(FsError),
}

impl core::fmt::Display for TarError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => f.write_str("archive is truncated"),
            Self::BadChecksum => f.write_str("header checksum mismatch"),
            Self::BadField => f.write_str("malformed header field"),
            Self::Fs(e) => write!(f, "{e}"),
        }
    }
}

/// What an archive entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// Anything else (links, devices, extended headers).
    Other,
}

/// One entry of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The path stored in the archive, with any `prefix` applied.
    pub path: String,
    /// The entry's type.
    pub kind: EntryKind,
    /// File contents; empty for directories.
    pub data: &'a [u8],
}

/// Parses a NUL- or space-terminated octal field.
fn octal(field: &[u8]) -> Result<u64, TarError> {
    let mut value: u64 = 0;
    let mut seen = false;
    for &byte in field {
        match byte {
            b'0'..=b'7' => {
                value = value
                    .checked_mul(8)
                    .and_then(|v| v.checked_add(u64::from(byte - b'0')))
                    .ok_or(TarError::BadField)?;
                seen = true;
            }
            b' ' if !seen => {}
            b' ' | 0 => break,
            _ => return Err(TarError::BadField),
        }
    }
    Ok(value)
}

/// The bytes of a NUL-padded text field.
fn text(field: &[u8]) -> Result<&str, TarError> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..end]).map_err(|_| TarError::BadField)
}

/// Iterates over the entries of `archive`. Iteration ends at the
/// end-of-archive marker (a zero block) or the end of the data; the first
/// error ends it too.
pub fn entries(archive: &[u8]) -> impl Iterator<Item = Result<Entry<'_>, TarError>> {
    let mut offset = 0;
    let mut failed = false;
    core::iter::from_fn(move || {
        if failed || offset >= archive.len() {
            return None;
        }
        let result = (|| {
            let header = archive
                .get(offset..offset + BLOCK)
                .ok_or(TarError::Truncated)?;
            if header.iter().all(|&b| b == 0) {
                return Ok(None);
            }
            // The checksum is the byte sum of the header with the checksum
            // field itself read as eight spaces.
            let sum: u64 = header
                .iter()
                .enumerate()
                .map(|(i, &b)| {
                    if (148..156).contains(&i) {
                        32
                    } else {
                        u64::from(b)
                    }
                })
                .sum();
            if octal(&header[148..156])? != sum {
                return Err(TarError::BadChecksum);
            }

            let size =
                usize::try_from(octal(&header[124..136])?).map_err(|_| TarError::BadField)?;
            let name = text(&header[..100])?;
            let prefix = text(&header[345..500])?;
            let mut path = String::new();
            if !prefix.is_empty() {
                path.push_str(prefix);
                path.push('/');
            }
            path.push_str(name);

            let kind = match header[156] {
                0 | b'0' => EntryKind::File,
                b'5' => EntryKind::Directory,
                _ => EntryKind::Other,
            };
            let start = offset + BLOCK;
            let data = archive
                .get(start..start.checked_add(size).ok_or(TarError::BadField)?)
                .ok_or(TarError::Truncated)?;
            offset = start + size.div_ceil(BLOCK) * BLOCK;
            Ok(Some(Entry {
                path,
                kind,
                data: if kind == EntryKind::File { data } else { &[] },
            }))
        })();
        match result {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => {
                failed = true;
                None
            }
            Err(error) => {
                failed = true;
                Some(Err(error))
            }
        }
    })
}

/// Extracts `archive` into `vfs` below the root directory, creating parent
/// directories as needed and replacing existing files. Returns the number
/// of files written.
pub fn unpack(vfs: &mut Vfs, archive: &[u8]) -> Result<usize, TarError> {
    let mut files = 0;
    for entry in entries(archive) {
        let entry = entry?;
        // Archive paths are relative; anchoring them at `/` and normalising
        // also neutralises `..` components.
        let path = normalize("/", &entry.path).map_err(TarError::Fs)?;
        match entry.kind {
            EntryKind::Directory => vfs.create_dir_all(&path).map_err(TarError::Fs)?,
            EntryKind::File => {
                if let Some((parent, _)) = crate::path::split(&path).map_err(TarError::Fs)? {
                    vfs.create_dir_all(parent).map_err(TarError::Fs)?;
                }
                vfs.write_file(&path, entry.data, WriteMode::Truncate)
                    .map_err(TarError::Fs)?;
                files += 1;
            }
            EntryKind::Other => {}
        }
    }
    Ok(files)
}

/// Builds one archive header block. Exposed for tests and for tools that
/// create archives; `kind` must not be [`EntryKind::Other`].
pub fn header(path: &str, kind: EntryKind, size: usize) -> Option<[u8; BLOCK]> {
    if path.len() > 100 || !path.is_ascii() || kind == EntryKind::Other {
        return None;
    }
    let mut block = [0u8; BLOCK];
    block[..path.len()].copy_from_slice(path.as_bytes());
    let mode: &[u8; 7] = if kind == EntryKind::Directory {
        b"0000755"
    } else {
        b"0000644"
    };
    block[100..107].copy_from_slice(mode);
    block[108..115].copy_from_slice(b"0000000"); // uid
    block[116..123].copy_from_slice(b"0000000"); // gid
    write_octal(&mut block[124..135], size as u64);
    write_octal(&mut block[136..147], 0); // mtime: none, for reproducibility
    block[156] = if kind == EntryKind::Directory {
        b'5'
    } else {
        b'0'
    };
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[148..156].fill(b' ');
    let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
    write_octal(&mut block[148..154], sum);
    block[154] = 0;
    block[155] = b' ';
    Some(block)
}

/// Writes `value` as zero-padded octal filling `field`.
fn write_octal(field: &mut [u8], mut value: u64) {
    for slot in field.iter_mut().rev() {
        *slot = b'0' + (value & 7) as u8;
        value >>= 3;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ramfs::RamFs;
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    fn archive(items: &[(&str, EntryKind, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (path, kind, data) in items {
            out.extend_from_slice(&header(path, *kind, data.len()).unwrap());
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
        out.resize(out.len() + 2 * BLOCK, 0);
        out
    }

    #[test]
    fn lists_entries() {
        let tar = archive(&[
            ("bin/", EntryKind::Directory, b""),
            ("bin/hello", EntryKind::File, b"\x7fELF..."),
            ("etc/motd", EntryKind::File, &[b'x'; 600]),
            ("empty", EntryKind::File, b""),
        ]);
        let list: Vec<_> = entries(&tar).map(Result::unwrap).collect();
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].path, "bin/");
        assert_eq!(list[0].kind, EntryKind::Directory);
        assert_eq!(list[1].data, b"\x7fELF...");
        assert_eq!(list[2].data.len(), 600);
        assert_eq!(list[3].data, b"");
    }

    #[test]
    fn unpacks_into_a_filesystem() {
        let tar = archive(&[
            ("bin/hello", EntryKind::File, b"program"),
            ("etc/conf.d/", EntryKind::Directory, b""),
            ("../../escape", EntryKind::File, b"contained"),
        ]);
        let mut vfs = Vfs::new(Box::new(RamFs::new()));
        assert_eq!(unpack(&mut vfs, &tar), Ok(2));
        assert_eq!(vfs.read_file("/bin/hello").unwrap(), b"program");
        assert!(vfs.read_dir("/etc/conf.d").unwrap().is_empty());
        assert_eq!(
            vfs.read_file("/escape").unwrap(),
            b"contained",
            "`..` cannot leave the root"
        );
    }

    #[test]
    fn detects_corruption() {
        let good = archive(&[("a", EntryKind::File, b"data")]);
        let mut bad = good.clone();
        bad[0] = b'b';
        assert_eq!(entries(&bad).next(), Some(Err(TarError::BadChecksum)));

        assert_eq!(
            entries(&good[..BLOCK + 2]).next(),
            Some(Err(TarError::Truncated))
        );
        assert_eq!(entries(&good[..100]).next(), Some(Err(TarError::Truncated)));

        let mut bad = good.clone();
        bad[124] = b'9';
        assert!(matches!(entries(&bad).next(), Some(Err(_))));

        // Errors end the iteration.
        let mut iter = entries(&bad);
        iter.next();
        assert!(iter.next().is_none());
        assert!(entries(&[]).next().is_none());
    }

    #[test]
    fn unsupported_headers_are_rejected_by_the_writer() {
        assert!(header(&"x".repeat(101), EntryKind::File, 0).is_none());
        assert!(header("link", EntryKind::Other, 0).is_none());
        assert!(header("naïve", EntryKind::File, 0).is_none());
    }
}
