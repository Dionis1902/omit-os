// SPDX-License-Identifier: GPL-2.0-only
//! Tests for the ext2 driver.
//!
//! Besides checking the driver against itself, images it produces are
//! verified with `e2fsck`, and files are exchanged with `debugfs` and
//! `mke2fs`, so the on-disk format is validated by an independent
//! implementation. Those checks are skipped (with a note on stderr) if the
//! e2fsprogs tools are not installed.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use std::process::Command;
use std::string::ToString;
use std::{eprintln, format, fs};

use omit_vfs::{FileSystem, FileType, FsError, Vfs, WriteMode};

use crate::{BlockIo, Ext2, Ext2Error, IoError, format};

/// A disk image in memory.
struct MemDisk(Vec<u8>);

impl MemDisk {
    fn new(megabytes: usize) -> Self {
        Self(vec![0u8; megabytes << 20])
    }
}

impl BlockIo for MemDisk {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), IoError> {
        let start = usize::try_from(offset).map_err(|_| IoError)?;
        let source = self.0.get(start..start + buffer.len()).ok_or(IoError)?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), IoError> {
        let start = usize::try_from(offset).map_err(|_| IoError)?;
        self.0
            .get_mut(start..start + data.len())
            .ok_or(IoError)?
            .copy_from_slice(data);
        Ok(())
    }
}

fn tool(name: &str) -> Option<String> {
    [
        "/usr/sbin",
        "/sbin",
        "/usr/bin",
        "/usr/local/sbin",
        "/opt/homebrew/sbin",
    ]
    .iter()
    .map(|dir| format!("{dir}/{name}"))
    .find(|path| fs::metadata(path).is_ok())
}

fn temp_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("omit-ext2-{}-{tag}.img", std::process::id()))
}

/// Runs `e2fsck -fn` over `image` and panics if it finds anything wrong.
fn fsck(image: &[u8], tag: &str) {
    let Some(e2fsck) = tool("e2fsck") else {
        eprintln!("note: e2fsck not found; skipping external check of `{tag}`");
        return;
    };
    let path = temp_path(tag);
    fs::write(&path, image).unwrap();
    let output = Command::new(e2fsck).arg("-fn").arg(&path).output().unwrap();
    let _ = fs::remove_file(&path);
    assert!(
        output.status.success(),
        "e2fsck found problems in `{tag}`:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fresh(megabytes: usize) -> Vfs {
    let mut disk = MemDisk::new(megabytes);
    format(&mut disk).unwrap();
    Vfs::new(Box::new(Ext2::mount(disk).unwrap()))
}

/// Formats, runs `actions`, and returns the resulting image.
fn build(megabytes: usize, actions: impl FnOnce(&mut Ext2<MemDisk>)) -> Vec<u8> {
    let mut disk = MemDisk::new(megabytes);
    format(&mut disk).unwrap();
    let mut filesystem = Ext2::mount(disk).unwrap();
    actions(&mut filesystem);
    filesystem.into_device().0
}

/// Deterministic test data that differs from block to block.
fn pattern(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add((i >> 8) as u8) ^ seed)
        .collect()
}

#[test]
fn a_fresh_filesystem_is_valid() {
    for megabytes in [1, 8, 32, 200] {
        let image = build(megabytes, |_| {});
        fsck(&image, &format!("fresh-{megabytes}"));
    }
    let vfs = fresh(8);
    let names: Vec<_> = vfs
        .read_dir("/")
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["lost+found"]);
}

#[test]
fn rejects_devices_that_are_not_ext2_or_too_small() {
    assert!(matches!(
        Ext2::mount(MemDisk::new(1)),
        Err(Ext2Error::NotExt2)
    ));
    let mut tiny = MemDisk(vec![0u8; 64 * 1024]);
    assert_eq!(format(&mut tiny), Err(Ext2Error::TooSmall));

    // An ext4-style feature flag must be refused, not misread.
    let mut disk = MemDisk::new(4);
    format(&mut disk).unwrap();
    disk.0[1024 + 96] |= 0x40; // INCOMPAT_EXTENTS
    assert!(matches!(
        Ext2::mount(disk),
        Err(Ext2Error::UnsupportedFeatures(_))
    ));
}

#[test]
fn files_and_directories_through_the_vfs() {
    let mut vfs = fresh(8);
    vfs.create_dir_all("/home/user/docs").unwrap();
    vfs.write_file("/home/user/a.txt", b"hello\n", WriteMode::Truncate)
        .unwrap();
    vfs.write_file("/home/user/a.txt", b"world\n", WriteMode::Append)
        .unwrap();
    assert_eq!(
        vfs.read_file("/home/user/a.txt").unwrap(),
        b"hello\nworld\n"
    );
    assert_eq!(vfs.metadata("/home/user/a.txt").unwrap().size, 12);
    assert_eq!(vfs.metadata("/home/user").unwrap().size, 2);
    assert_eq!(
        vfs.metadata("/home/user").unwrap().file_type,
        FileType::Directory
    );

    assert_eq!(vfs.create_dir("/home"), Err(FsError::AlreadyExists));
    assert_eq!(vfs.remove("/home"), Err(FsError::DirectoryNotEmpty));
    assert_eq!(vfs.read_file("/home"), Err(FsError::IsADirectory));
    assert_eq!(
        vfs.read_dir("/home/user/a.txt"),
        Err(FsError::NotADirectory)
    );
    assert_eq!(vfs.metadata("/nope"), Err(FsError::NotFound));

    vfs.copy_file("/home/user/a.txt", "/home/user/docs/b.txt")
        .unwrap();
    vfs.rename("/home/user/docs", "/docs").unwrap();
    assert_eq!(vfs.read_file("/docs/b.txt").unwrap(), b"hello\nworld\n");
    assert_eq!(
        vfs.rename("/home", "/home/user/inside"),
        Err(FsError::InvalidArgument)
    );
    assert_eq!(vfs.rename("/docs", "/home"), Err(FsError::AlreadyExists));

    vfs.remove_all("/home").unwrap();
    vfs.remove_all("/docs").unwrap();
    let names: Vec<_> = vfs
        .read_dir("/")
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["lost+found"]);
}

#[test]
fn large_and_sparse_files() {
    let big = pattern(6 << 20, 7); // direct + single + double indirect
    let image = build(32, |fs| {
        let root = fs.root();
        let file = fs.create(root, "big", FileType::File).unwrap();
        assert_eq!(fs.write(file, 0, &big), Ok(big.len()));
        let mut back = vec![0u8; big.len()];
        assert_eq!(fs.read(file, 0, &mut back), Ok(big.len()));
        assert!(back == big, "large file read back differs");

        // Unaligned overwrite spanning a block boundary.
        assert_eq!(fs.write(file, 4090, b"0123456789"), Ok(10));
        let mut window = [0u8; 12];
        assert_eq!(fs.read(file, 4089, &mut window), Ok(12));
        assert_eq!(&window[1..11], b"0123456789");
        assert_eq!(window[0], big[4089]);

        // A hole: writing far past the end leaves zeros in between.
        let sparse = fs.create(root, "sparse", FileType::File).unwrap();
        assert_eq!(fs.write(sparse, 3 << 20, b"end"), Ok(3));
        assert_eq!(fs.metadata(sparse).unwrap().size, (3 << 20) + 3);
        let mut hole = [0xFFu8; 64];
        assert_eq!(fs.read(sparse, 1 << 20, &mut hole), Ok(64));
        assert!(hole.iter().all(|&b| b == 0));
        let mut end = [0u8; 8];
        assert_eq!(fs.read(sparse, 3 << 20, &mut end), Ok(3));
        assert_eq!(&end[..3], b"end");
    });
    fsck(&image, "large");
}

#[test]
fn truncation_returns_space() {
    let mut disk = MemDisk::new(32);
    format(&mut disk).unwrap();
    let mut fs = Ext2::mount(disk).unwrap();
    let free_at_start = fs.free_bytes();
    let root = fs.root();
    let file = fs.create(root, "f", FileType::File).unwrap();
    let data = pattern(5 << 20, 1);
    fs.write(file, 0, &data).unwrap();
    assert!(fs.free_bytes() < free_at_start - (5 << 20));

    // Shrink into the single-indirect range, mid-block.
    fs.truncate(file, 100_000).unwrap();
    assert_eq!(fs.metadata(file).unwrap().size, 100_000);
    let mut back = vec![0u8; 100_000];
    assert_eq!(fs.read(file, 0, &mut back), Ok(100_000));
    assert!(back == data[..100_000]);
    // Growing again exposes zeros, not the old contents.
    fs.truncate(file, 200_000).unwrap();
    let mut tail = vec![0xFFu8; 100_000];
    assert_eq!(fs.read(file, 100_000, &mut tail), Ok(100_000));
    assert!(tail.iter().all(|&b| b == 0));

    fs.truncate(file, 0).unwrap();
    fs.remove(root, "f").unwrap();
    assert_eq!(fs.free_bytes(), free_at_start, "all blocks returned");
    fsck(&fs.into_device().0, "truncate");
}

#[test]
fn many_entries_grow_and_shrink_directories() {
    let image = build(16, |fs| {
        let root = fs.root();
        let dir = fs.create(root, "many", FileType::Directory).unwrap();
        for index in 0..600 {
            let name = format!("file-with-a-fairly-long-name-{index:04}");
            let file = fs.create(dir, &name, FileType::File).unwrap();
            fs.write(file, 0, name.as_bytes()).unwrap();
        }
        assert_eq!(fs.read_dir(dir).unwrap().len(), 600);
        assert_eq!(
            fs.create(dir, "file-with-a-fairly-long-name-0300", FileType::File),
            Err(FsError::AlreadyExists)
        );
        // Remove every other entry, then reuse the gaps.
        for index in (0..600).step_by(2) {
            fs.remove(dir, &format!("file-with-a-fairly-long-name-{index:04}"))
                .unwrap();
        }
        for index in 0..150 {
            fs.create(dir, &format!("d{index}"), FileType::Directory)
                .unwrap();
        }
        let entries = fs.read_dir(dir).unwrap();
        assert_eq!(entries.len(), 450);
        assert!(entries.windows(2).all(|pair| pair[0].name < pair[1].name));

        let survivor = fs.lookup(dir, "file-with-a-fairly-long-name-0301").unwrap();
        let mut content = [0u8; 64];
        let length = fs.read(survivor, 0, &mut content).unwrap();
        assert_eq!(&content[..length], b"file-with-a-fairly-long-name-0301");
    });
    fsck(&image, "directories");
}

#[test]
fn renames_keep_link_counts_right() {
    let image = build(8, |fs| {
        let root = fs.root();
        let a = fs.create(root, "a", FileType::Directory).unwrap();
        let b = fs.create(root, "b", FileType::Directory).unwrap();
        let sub = fs.create(a, "sub", FileType::Directory).unwrap();
        let file = fs.create(sub, "f", FileType::File).unwrap();
        fs.write(file, 0, b"payload").unwrap();

        fs.rename(a, "sub", b, "moved").unwrap(); // across directories
        fs.rename(b, "moved", b, "renamed").unwrap(); // within one
        fs.rename(sub, "f", root, "top").unwrap(); // a file, upwards
        assert_eq!(
            fs.rename(root, "b", sub, "x"),
            Err(FsError::InvalidArgument)
        );
        assert_eq!(fs.lookup(b, "renamed"), Ok(sub));
        assert_eq!(fs.lookup(root, "top"), Ok(file));
        assert_eq!(fs.lookup(a, "sub"), Err(FsError::NotFound));
        fs.remove(root, "a").unwrap();
    });
    // e2fsck verifies every link count and `..` entry.
    fsck(&image, "rename");
}

#[test]
fn running_out_of_space_is_reported_and_recoverable() {
    let mut disk = MemDisk::new(2);
    format(&mut disk).unwrap();
    let mut fs = Ext2::mount(disk).unwrap();
    let root = fs.root();
    let file = fs.create(root, "fill", FileType::File).unwrap();
    let chunk = pattern(64 * 1024, 3);
    let mut written = 0u64;
    loop {
        match fs.write(file, written, &chunk) {
            Ok(count) => written += count as u64,
            Err(FsError::NoSpace) => break,
            Err(other) => panic!("unexpected error {other:?}"),
        }
        assert!(written < 4 << 20, "wrote more than the disk holds");
    }
    assert!(written > 1 << 20);
    assert_eq!(
        fs.create(root, "dir", FileType::Directory),
        Err(FsError::NoSpace)
    );
    fsck(&fs.into_device().0.clone(), "full");
}

#[test]
fn data_survives_a_remount() {
    let payload = pattern(300_000, 9);
    let image = build(8, |fs| {
        let root = fs.root();
        let dir = fs.create(root, "keep", FileType::Directory).unwrap();
        let file = fs.create(dir, "data.bin", FileType::File).unwrap();
        fs.write(file, 0, &payload).unwrap();
    });
    let vfs = Vfs::new(Box::new(Ext2::mount(MemDisk(image)).unwrap()));
    assert!(vfs.read_file("/keep/data.bin").unwrap() == payload);
}

/// Files written by this driver must be readable by e2fsprogs, and a
/// filesystem created and populated by e2fsprogs must be readable (and
/// writable) by this driver.
#[test]
fn interoperates_with_e2fsprogs() {
    let (Some(debugfs), Some(mke2fs)) = (tool("debugfs"), tool("mke2fs")) else {
        eprintln!("note: e2fsprogs not found; skipping interoperability test");
        return;
    };
    let payload = pattern(150_000, 5);

    // Ours -> theirs.
    let image = build(8, |fs| {
        let root = fs.root();
        let dir = fs.create(root, "from-omit", FileType::Directory).unwrap();
        let file = fs.create(dir, "payload.bin", FileType::File).unwrap();
        fs.write(file, 0, &payload).unwrap();
    });
    let path = temp_path("interop-out");
    fs::write(&path, &image).unwrap();
    let output = Command::new(&debugfs)
        .args(["-R", "cat /from-omit/payload.bin"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = fs::remove_file(&path);
    assert!(output.stdout == payload, "debugfs read different data");

    // Theirs -> ours, for each block size.
    for block_size in ["1024", "2048", "4096"] {
        let path = temp_path(&format!("interop-in-{block_size}"));
        let source = temp_path(&format!("interop-src-{block_size}"));
        fs::write(&path, vec![0u8; 8 << 20]).unwrap();
        fs::write(&source, &payload).unwrap();
        let status = Command::new(&mke2fs)
            .args(["-q", "-t", "ext2", "-b", block_size, "-F"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let script = format!(
            "mkdir /theirs\nwrite {} /theirs/payload.bin\n",
            source.display()
        );
        let script_path = temp_path(&format!("interop-script-{block_size}"));
        fs::write(&script_path, script).unwrap();
        let status = Command::new(&debugfs)
            .arg("-w")
            .arg("-f")
            .arg(&script_path)
            .arg(&path)
            .output()
            .unwrap()
            .status;
        assert!(status.success());

        let disk = MemDisk(fs::read(&path).unwrap());
        let mut vfs = Vfs::new(Box::new(Ext2::mount(disk).unwrap()));
        assert!(
            vfs.read_file("/theirs/payload.bin").unwrap() == payload,
            "block size {block_size}: read different data"
        );
        // Modify their filesystem and have their checker judge the result.
        vfs.write_file(
            "/theirs/added.txt",
            b"written by omit\n",
            WriteMode::Truncate,
        )
        .unwrap();
        vfs.create_dir("/ours").unwrap();
        vfs.write_file("/ours/big.bin", &pattern(900_000, 2), WriteMode::Truncate)
            .unwrap();
        vfs.remove("/theirs/payload.bin").unwrap();
        drop(vfs);
        for file in [&path, &source, &script_path] {
            let _ = fs::remove_file(file);
        }
        let _ = block_size.to_string();
    }
}

/// The same check as above, but keeping the modified image so that
/// `e2fsck` can inspect what this driver did to a foreign filesystem.
#[test]
fn modifying_a_foreign_filesystem_keeps_it_consistent() {
    let Some(mke2fs) = tool("mke2fs") else {
        eprintln!("note: mke2fs not found; skipping");
        return;
    };
    for block_size in ["1024", "4096"] {
        let path = temp_path(&format!("foreign-{block_size}"));
        fs::write(&path, vec![0u8; 16 << 20]).unwrap();
        let status = Command::new(&mke2fs)
            .args(["-q", "-t", "ext2", "-b", block_size, "-F"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let mut filesystem = Ext2::mount(MemDisk(fs::read(&path).unwrap())).unwrap();
        let _ = fs::remove_file(&path);

        let root = filesystem.root();
        let dir = filesystem
            .create(root, "work", FileType::Directory)
            .unwrap();
        for index in 0..40 {
            let file = filesystem
                .create(dir, &format!("f{index}"), FileType::File)
                .unwrap();
            filesystem
                .write(file, 0, &pattern(index * 9000, index as u8))
                .unwrap();
        }
        for index in (0..40).step_by(3) {
            filesystem.remove(dir, &format!("f{index}")).unwrap();
        }
        let big = filesystem.lookup(dir, "f38").unwrap();
        filesystem.truncate(big, 5000).unwrap();
        filesystem.rename(dir, "f37", root, "moved").unwrap();
        fsck(
            &filesystem.into_device().0,
            &format!("foreign-{block_size}"),
        );
    }
}
