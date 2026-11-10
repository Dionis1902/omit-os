// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! `cargo xtask` — the Omit OS build, image, run and test driver.
//!
//! Everything that goes beyond a plain `cargo build` lives here, in Rust,
//! so that building and testing the OS needs no shell scripts, Makefiles or
//! host tools other than Cargo and QEMU. Run `cargo xtask help` for usage.

mod gpt;
mod iso;

use std::error::Error;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "\
Omit OS build driver

USAGE:
    cargo xtask <COMMAND> [OPTIONS]

COMMANDS:
    build    Build the boot loader and the kernel
    image    Build a bootable UEFI disk image (with --iso: an ISO image)
    dist     Build the release artifacts (disk image and ISO) in target/omit/dist
    run      Build and boot the image in QEMU (serial console on stdio)
    test     Build and run the QEMU integration tests
    help     Show this message

OPTIONS:
    --release           Build with optimisations
    --cmdline <TEXT>    Kernel command line to embed in the image (image, run)
    --cpu <MODEL>       QEMU CPU model (run, test) [default: max]
    --memory <SIZE>     QEMU guest memory (run, test) [default: 256M]
    --accel <NAME>      QEMU accelerator, e.g. kvm (run, test) [default: tcg]
    --only <TEST>       Run only the named boot test (test)
    --iso               Use a bootable ISO instead of a disk image (image, run, test)
    --gdb               Start QEMU paused with a GDB server on :1234 (run)

ENVIRONMENT:
    QEMU                 QEMU binary [default: qemu-system-x86_64]
    OVMF_CODE, OVMF_VARS UEFI firmware images (auto-detected if unset)
    OMIT_TEST_PORT_OFFSET
                         Added to every port on this machine that the
                         network tests use, so that two runs do not
                         collide [default: 0]
    OMIT_TEST_TIMEOUT    Per-test timeout in seconds [default: 120]
                         (the desktop tests get three times as long)
";

/// Size of the generated disk image.
const IMAGE_SIZE: u64 = 48 << 20;

/// QEMU exit status produced by the kernel writing `0x10` to the
/// isa-debug-exit device: `(0x10 << 1) | 1`.
const QEMU_EXIT_SUCCESS: i32 = 33;
/// As above for `0x11`.
const QEMU_EXIT_FAILURE: i32 = 35;

const SUCCESS_MESSAGE: &str = "Omit OS — Kernel initialized successfully.";

#[derive(Debug, Default)]
struct Options {
    release: bool,
    gdb: bool,
    cmdline: String,
    cpu: Option<String>,
    memory: Option<String>,
    accel: Option<String>,
    /// Boot from the ISO image instead of the disk image.
    iso: bool,
    /// Run only the boot test with this name.
    only: Option<String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self::default();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            let mut value = |name: &str| {
                iter.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} requires a value"))
            };
            match arg.as_str() {
                "--release" => options.release = true,
                "--gdb" => options.gdb = true,
                "--iso" => options.iso = true,
                "--cmdline" => options.cmdline = value("--cmdline")?,
                "--cpu" => options.cpu = Some(value("--cpu")?),
                "--memory" => options.memory = Some(value("--memory")?),
                "--accel" => options.accel = Some(value("--accel")?),
                "--only" => options.only = Some(value("--only")?),
                other => return Err(format!("unknown option `{other}`").into()),
            }
        }
        Ok(options)
    }

    fn profile_dir(&self) -> &'static str {
        if self.release { "release" } else { "debug" }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask lives in <root>/tools/xtask")
        .to_path_buf()
}

fn out_dir(options: &Options) -> Result<PathBuf> {
    let dir = workspace_root()
        .join("target/omit")
        .join(options.profile_dir());
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Paths of the build products that go onto the EFI system partition.
struct Artifacts {
    loader: PathBuf,
    /// The kernel as it goes onto boot media: the ELF file reduced to
    /// what the boot loader reads (see [`strip_elf`]).
    kernel_image: Vec<u8>,
    /// The initial ramdisk: a tar archive of the user programs.
    initrd: Vec<u8>,
}

/// Reduces an ELF64 executable to its header, program headers and the
/// contents of its segments: everything a loader needs. Section headers,
/// symbol tables and debugging information — most of a debug build — are
/// dropped. The full file stays in `target/` for debuggers and
/// `addr2line`.
fn strip_elf(file: &[u8]) -> Result<Vec<u8>> {
    let u16_at = |offset: usize| -> Result<u64> {
        let bytes = file.get(offset..offset + 2).ok_or("truncated ELF file")?;
        Ok(u64::from(u16::from_le_bytes([bytes[0], bytes[1]])))
    };
    let u64_at = |offset: usize| -> Result<u64> {
        let bytes = file.get(offset..offset + 8).ok_or("truncated ELF file")?;
        let mut value = [0u8; 8];
        value.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(value))
    };
    if file.get(..5) != Some(b"\x7FELF\x02") {
        return Err("not an ELF64 file".into());
    }
    let program_headers = u64_at(0x20)?;
    let entry_size = u16_at(0x36)?;
    let entry_count = u16_at(0x38)?;
    let mut end = program_headers + entry_size * entry_count;
    for index in 0..entry_count {
        let header = (program_headers + index * entry_size) as usize;
        // Offset and size in the file of the segment's contents.
        end = end.max(u64_at(header + 0x08)? + u64_at(header + 0x20)?);
    }
    let mut stripped = file
        .get(..end as usize)
        .ok_or("ELF segments extend past the end of the file")?
        .to_vec();
    // No section header table: offset, entry count and string table index.
    stripped[0x28..0x30].fill(0);
    stripped[0x3C..0x40].fill(0);
    Ok(stripped)
}

/// User programs shipped in `/bin` of the initial ramdisk.
const USER_PROGRAMS: &[&str] = &[
    "cat",
    "clock",
    "crash",
    "desktop",
    "echo",
    "echod",
    "fetch",
    "fileops",
    "hello",
    "host",
    "ipctest",
    "ls",
    "memtest",
    "nettest",
    "pipetest",
    "pop",
    "random",
    "run",
    "scribble",
    "sleeper",
    "spin",
    "syscheck",
    "tee",
    "tftp",
    "threadtest",
    "winrun",
];

/// Windows test programs (see `compat/windows/samples`) shipped in `/bin`.
const WINDOWS_PROGRAMS: &[&str] = &[
    "windll.exe",
    "winfloat.exe",
    "wingui.exe",
    "winhello.exe",
    "winmissing.exe",
    "winplugin.dll",
    "winsample.dll",
    "wintest.exe",
    "winthread.exe",
];

/// The target the Windows test programs are built for.
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

/// Builds the Windows test programs: real PE32+ console executables,
/// linked with the LLVM linker that ships with Rust (no Windows SDK),
/// without the C runtime. Always optimised: they are test inputs, not
/// something to debug, and one of them relies on values staying in
/// registers.
fn build_windows_programs() -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let flags = [
        "-Clinker-flavor=lld-link",
        "-Cpanic=abort",
        "-Clink-arg=/NODEFAULTLIB",
        // Reproducible output; see the loader's flags in .cargo/config.toml.
        "-Clink-arg=/Brepro",
        "-Clink-arg=/DEBUG:NONE",
    ];
    let status = Command::new(cargo)
        .current_dir(workspace_root())
        .env("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER", "rust-lld")
        .env(
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS",
            flags.join(" "),
        )
        .args(["build", "--release", "--target", WINDOWS_TARGET])
        // The entry point and subsystem are set by each package's build
        // script: they differ between programs and libraries.
        .args(["--package", "omit-win-samples"])
        .args(["--package", "omit-win-sample-dll"])
        .args(["--package", "omit-win-sample-plugin"])
        .status()?;
    if !status.success() {
        return Err("building the Windows test programs failed".into());
    }
    Ok(())
}

/// Packs `files` (archive path, contents) into a `ustar` archive with no
/// timestamps, so identical inputs give identical output.
fn build_tar(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    use omit_vfs::tar::{EntryKind, header};
    let mut archive = Vec::new();
    for (path, data) in files {
        let block = header(path, EntryKind::File, data.len())
            .ok_or_else(|| format!("cannot archive `{path}`"))?;
        archive.extend_from_slice(&block);
        archive.extend_from_slice(data);
        archive.resize(archive.len().div_ceil(512) * 512, 0);
    }
    // End-of-archive marker: two zero blocks.
    archive.resize(archive.len() + 1024, 0);
    Ok(archive)
}

fn cargo_build(package: &str, target: &str, options: &Options) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .current_dir(workspace_root())
        .args(["build", "--package", package, "--target", target]);
    if options.release {
        command.arg("--release");
    }
    if !command.status()?.success() {
        return Err(format!("building {package} failed").into());
    }
    Ok(())
}

fn build(options: &Options) -> Result<Artifacts> {
    cargo_build("omit-boot", "x86_64-unknown-uefi", options)?;
    cargo_build("omit-kernel", "x86_64-unknown-none", options)?;
    cargo_build("omit-user-programs", "x86_64-unknown-none", options)?;
    let target = workspace_root().join("target");
    let user_dir = target
        .join("x86_64-unknown-none")
        .join(options.profile_dir());
    let mut files = Vec::new();
    for program in USER_PROGRAMS {
        files.push((format!("bin/{program}"), fs::read(user_dir.join(program))?));
    }
    // The installer copies the boot loader and the kernel from the running
    // system, so they are part of it.
    let loader = target
        .join("x86_64-unknown-uefi")
        .join(options.profile_dir())
        .join("omit-boot.efi");
    let kernel_image = strip_elf(&fs::read(
        target
            .join("x86_64-unknown-none")
            .join(options.profile_dir())
            .join("omit-kernel"),
    )?)?;
    files.push(("boot/BOOTX64.EFI".into(), fs::read(&loader)?));
    files.push(("boot/KERNEL.ELF".into(), kernel_image.clone()));
    build_windows_programs()?;
    let windows_dir = target.join(WINDOWS_TARGET).join("release");
    for program in WINDOWS_PROGRAMS {
        files.push((
            format!("bin/{program}"),
            fs::read(windows_dir.join(program))?,
        ));
    }
    Ok(Artifacts {
        loader,
        kernel_image,
        initrd: build_tar(&files)?,
    })
}

/// Size of the FAT boot image embedded in the ISO.
const ISO_BOOT_IMAGE_SIZE: usize = 24 << 20;

/// Text of the `README.TXT` placed on the ISO.
const ISO_README: &str = "\
Omit OS live image.

This disc boots on x86-64 machines with UEFI firmware (Secure Boot off).
It runs entirely from memory and modifies nothing on the machine unless
you run its `install` command, which erases the disk you name.
See https://github.com/dionis1902/omit-os
";

/// Formats `volume` as FAT and fills it with the EFI system partition
/// contents: the loader, the kernel and (if non-empty) the kernel command
/// line. Volume identifiers are fixed and no timestamps are recorded, so
/// the result is reproducible.
fn write_esp(
    volume: &mut [u8],
    fat_type: Option<fatfs::FatType>,
    artifacts: &Artifacts,
    cmdline: &str,
) -> Result<()> {
    if !cmdline.is_ascii() || cmdline.len() > 1024 {
        return Err("kernel command line must be ASCII and at most 1024 bytes".into());
    }
    let mut volume = Cursor::new(volume);
    let mut format = fatfs::FormatVolumeOptions::new()
        .volume_id(0x4F4D_4954) // "OMIT"
        .volume_label(*b"OMIT OS    ");
    if let Some(fat_type) = fat_type {
        format = format.fat_type(fat_type);
    }
    fatfs::format_volume(&mut volume, format)?;

    let fs = fatfs::FileSystem::new(&mut volume, fatfs::FsOptions::new())?;
    let root = fs.root_dir();
    let efi = root.create_dir("EFI")?;
    let boot = efi.create_dir("BOOT")?;
    let omit = efi.create_dir("OMIT")?;

    let put = |dir: &fatfs::Dir<'_, _>, name: &str, data: &[u8]| -> Result<()> {
        let mut file = dir.create_file(name)?;
        file.truncate()?;
        file.write_all(data)?;
        Ok(())
    };
    // The removable-media default path: firmware boots it with no
    // boot-manager configuration.
    put(&boot, "BOOTX64.EFI", &fs::read(&artifacts.loader)?)?;
    put(&omit, "KERNEL.ELF", &artifacts.kernel_image)?;
    put(&omit, "INITRD", &artifacts.initrd)?;
    if !cmdline.is_empty() {
        put(&omit, "CMDLINE", cmdline.as_bytes())?;
    }
    Ok(())
}

/// Builds a GPT disk image with a FAT32 EFI System Partition. This is the
/// image to write to a USB stick or attach as a hard disk.
fn build_image(artifacts: &Artifacts, cmdline: &str, path: &Path) -> Result<()> {
    let mut disk = vec![0u8; IMAGE_SIZE as usize];
    gpt::write_partition_table(&mut disk);
    let esp = gpt::esp_extent(IMAGE_SIZE);
    write_esp(
        &mut disk[esp.offset as usize..(esp.offset + esp.size) as usize],
        Some(fatfs::FatType::Fat32),
        artifacts,
        cmdline,
    )?;
    fs::write(path, &disk)?;
    Ok(())
}

/// Builds a UEFI-bootable ISO 9660 image (El Torito, EFI platform) for
/// optical media and virtual machines.
fn build_iso(artifacts: &Artifacts, cmdline: &str, path: &Path) -> Result<()> {
    let mut boot_image = vec![0u8; ISO_BOOT_IMAGE_SIZE];
    write_esp(&mut boot_image, None, artifacts, cmdline)?;
    fs::write(path, iso::build(&boot_image, "OMIT_OS", ISO_README))?;
    Ok(())
}

/// Size of the data disk attached to the virtual machine.
const DATA_DISK_SIZE: u64 = 128 << 20;

/// Creates an empty (all-zero) data disk image at `path`, replacing any
/// existing file.
fn create_data_disk(path: &Path) -> Result<()> {
    let file = fs::File::create(path)?;
    file.set_len(DATA_DISK_SIZE)?;
    Ok(())
}

/// Locates the OVMF firmware code and variable-store images.
fn find_ovmf() -> Result<(PathBuf, PathBuf)> {
    if let (Ok(code), Ok(vars)) = (std::env::var("OVMF_CODE"), std::env::var("OVMF_VARS")) {
        return Ok((code.into(), vars.into()));
    }
    const CANDIDATES: &[(&str, &str)] = &[
        // Debian / Ubuntu
        (
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
        ),
        (
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/OVMF/OVMF_VARS.fd",
        ),
        // Fedora
        (
            "/usr/share/edk2/ovmf/OVMF_CODE.fd",
            "/usr/share/edk2/ovmf/OVMF_VARS.fd",
        ),
        // Arch
        (
            "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
            "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
        ),
        (
            "/usr/share/edk2-ovmf/x64/OVMF_CODE.fd",
            "/usr/share/edk2-ovmf/x64/OVMF_VARS.fd",
        ),
        // Homebrew QEMU
        (
            "/opt/homebrew/share/qemu/edk2-x86_64-code.fd",
            "/opt/homebrew/share/qemu/edk2-i386-vars.fd",
        ),
        (
            "/usr/local/share/qemu/edk2-x86_64-code.fd",
            "/usr/local/share/qemu/edk2-i386-vars.fd",
        ),
    ];
    CANDIDATES
        .iter()
        .find(|(code, vars)| Path::new(code).exists() && Path::new(vars).exists())
        .map(|(code, vars)| (PathBuf::from(code), PathBuf::from(vars)))
        .ok_or_else(|| {
            "OVMF firmware not found; install it (e.g. `apt install ovmf`) \
             or set OVMF_CODE and OVMF_VARS"
                .into()
        })
}

/// Builds the QEMU command line for booting `image`.
///
/// `vars` is a private, writable copy of the firmware variable store.
/// `monitor`, if given, is the path of a Unix socket on which QEMU offers
/// its human monitor (used by tests to press keys and take screenshots).
fn qemu_command(
    image: Option<&Path>,
    vars: &Path,
    data_disk: &Path,
    monitor: Option<&Path>,
    extra: &[&str],
    options: &Options,
) -> Result<Command> {
    // Without a boot medium the machine has to start from the data disk.
    let media = image.map(|image| {
        if options.iso {
            format!(
                "format=raw,media=cdrom,readonly=on,file={}",
                image.display()
            )
        } else {
            format!("format=raw,file={}", image.display())
        }
    });
    let (code, vars_template) = find_ovmf()?;
    fs::copy(vars_template, vars)?;

    let qemu = std::env::var("QEMU").unwrap_or_else(|_| "qemu-system-x86_64".into());
    let mut command = Command::new(qemu);
    command
        .args(["-machine", "q35"])
        .args(["-accel", options.accel.as_deref().unwrap_or("tcg")])
        .args(["-cpu", options.cpu.as_deref().unwrap_or("max")])
        .args(["-m", options.memory.as_deref().unwrap_or("256M")])
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            code.display()
        ))
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=1,file={}",
            vars.display()
        ))
        // A VirtIO disk for persistent data, separate from the boot medium.
        .arg("-drive")
        .arg(format!(
            "if=none,id=data,format=raw,file={}",
            data_disk.display()
        ))
        .args(["-device", "virtio-blk-pci,drive=data"])
        // A hardware watchdog, driven by a reused Linux driver. When it
        // expires it resets the machine (which, with -no-reboot, ends QEMU).
        .args(["-device", "i6300esb"])
        .args(["-action", "watchdog=reset"])
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        // A fixed date and time, advancing with the guest's own clock,
        // so that what the tests see does not depend on when they run.
        .args(["-rtc", "base=2026-11-10T12:00:00,clock=vm"])
        .args(["-display", "none"])
        .args(["-nic", "none"])
        .arg("-no-reboot")
        .args(extra);
    // The serial console on standard input and output — unless the test
    // says what the machine's serial port is (or that it has none).
    if !extra.contains(&"-serial") {
        command.args(["-serial", "stdio"]);
    }
    if let Some(media) = media {
        command.arg("-drive").arg(media);
    }
    match monitor {
        Some(socket) => {
            let _ = fs::remove_file(socket);
            command
                .arg("-monitor")
                .arg(format!("unix:{},server,nowait", socket.display()));
        }
        None => {
            command.args(["-monitor", "none"]);
        }
    }
    Ok(command)
}

/// Builds the boot medium selected by `options` (disk image, or ISO with
/// `--iso`) at `path`.
fn build_media(artifacts: &Artifacts, cmdline: &str, path: &Path, options: &Options) -> Result<()> {
    if options.iso {
        build_iso(artifacts, cmdline, path)
    } else {
        build_image(artifacts, cmdline, path)
    }
}

fn cmd_image(options: &Options) -> Result<PathBuf> {
    let artifacts = build(options)?;
    let name = if options.iso {
        "omit-os.iso"
    } else {
        "omit-os.img"
    };
    let image = out_dir(options)?.join(name);
    build_media(&artifacts, &options.cmdline, &image, options)?;
    Ok(image)
}

/// Builds the release artifacts: disk image and ISO, named after the
/// version and architecture, in `target/omit/dist/`.
fn cmd_dist(options: &Options) -> Result<()> {
    let options = Options {
        release: true,
        cmdline: options.cmdline.clone(),
        ..Options::default()
    };
    let artifacts = build(&options)?;
    let dir = workspace_root().join("target/omit/dist");
    fs::create_dir_all(&dir)?;
    let stem = format!("omit-os-{}-x86_64", env!("CARGO_PKG_VERSION"));
    let image = dir.join(format!("{stem}.img"));
    let iso = dir.join(format!("{stem}.iso"));
    build_image(&artifacts, &options.cmdline, &image)?;
    build_iso(&artifacts, &options.cmdline, &iso)?;
    println!("{}", image.display());
    println!("{}", iso.display());
    Ok(())
}

fn cmd_run(options: &Options) -> Result<ExitCode> {
    let image = cmd_image(options)?;
    let vars = out_dir(options)?.join("ovmf-vars.fd");
    // The data disk of `xtask run` persists between runs.
    let data_disk = out_dir(options)?.join("omit-data.img");
    if !data_disk.exists() {
        create_data_disk(&data_disk)?;
    }
    let mut command = qemu_command(Some(&image), &vars, &data_disk, None, &[], options)?;
    if options.gdb {
        command.args(["-s", "-S"]);
        eprintln!("xtask: QEMU is paused; attach with `gdb -ex 'target remote :1234'`");
    }
    let status = command.status()?;
    // With `qemu-exit` on the command line the kernel reports its status
    // through the debug-exit device; translate it. Otherwise QEMU was simply
    // closed by the user.
    Ok(match status.code() {
        Some(0) | Some(QEMU_EXIT_SUCCESS) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    })
}

/// One QEMU boot test.
struct BootTest {
    name: &'static str,
    /// Kernel command line (the harness adds `qemu-exit`).
    cmdline: &'static str,
    /// Exit status QEMU must terminate with.
    expected_exit: i32,
    /// Strings that must each appear in the serial log.
    must_contain: &'static [&'static str],
    /// Strings that must not appear in the serial log.
    must_not_contain: &'static [&'static str],
    /// Shell session to drive over the serial console: `(input line, text
    /// that must appear in the output it produces)`. When non-empty the
    /// kernel is booted into its shell, each line is typed once the prompt
    /// appears, and `exit` is sent at the end. Output is matched with line
    /// endings normalised to `\n`.
    script: &'static [(&'static str, &'static str)],
    /// A second shell session, run after rebooting the machine with the
    /// same data disk. Used to test that data persists. When non-empty,
    /// the data disk is also checked with `e2fsck` afterwards.
    reboot_script: &'static [(&'static str, &'static str)],
    /// The test ends with the machine being reset rather than with the
    /// shell exiting: after the script, wait for QEMU to terminate instead
    /// of typing `exit`.
    ends_with_reset: bool,
    /// Also test the screen and keyboard: after the script, type a command
    /// on the emulated PS/2 keyboard, check that it runs, then take a
    /// screenshot and check what the framebuffer console drew.
    display: bool,
    /// After that, start the graphical desktop and operate it with the
    /// emulated mouse and keyboard, checking screenshots (see
    /// [`desktop_scenario`]). Requires `display`.
    desktop: bool,
    /// Additional QEMU arguments, for devices only this test needs.
    qemu_args: &'static [&'static str],
    /// The reboot (see `reboot_script`) starts from the data disk alone,
    /// without the boot medium: the first boot is expected to have
    /// installed the system onto it. Afterwards the disk is checked as an
    /// installed disk (see [`check_installed_disk`]) instead of as one
    /// ext2 filesystem.
    boot_installed: bool,
    /// Attach a second, empty disk behind an NVMe controller. With
    /// `boot_installed`, it is the disk the system is expected to have
    /// been installed on.
    nvme: bool,
    /// Like `nvme`, with a SATA disk on port 2 of the machine's AHCI
    /// controller (`sdc`).
    sata: bool,
    /// The machine has no serial port (`-serial none` in `qemu_args`), so
    /// there is no log and no way to type a script: the test waits for
    /// the console to appear on the screen, types `exit` on the keyboard
    /// and expects the kernel to report success. Requires `display`.
    no_serial: bool,
    /// Attach a network card of this QEMU model on QEMU's built-in
    /// user-mode network, with a TFTP server (see [`prepare_tftp_files`])
    /// and, on the host, a name server, two web servers and an echo
    /// client for the test (see [`start_test_name_server`] and what
    /// follows it).
    network: Option<&'static str>,
}

const BOOT_TESTS: &[BootTest] = &[
    BootTest {
        name: "boot",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] time: real-time clock reads 2026-11-10 12:00:",
            "tick 100 Hz; measured against the PIT",
            "[INFO ] time: a second reference clock differs by ",
            SUCCESS_MESSAGE,
            "[INFO ] serial: COM1 16550 UART ready",
            "[INFO ] cpu: IDT loaded, 256 vectors installed, #BP self-test passed",
            "[INFO ] paging: identity map removed",
            "[INFO ] mem: frame allocator ready",
            "[INFO ] mem: kernel heap ready: self-test passed",
            "[INFO ] paging: map/unmap self-test passed",
            "[INFO ] irq: legacy PIC masked; local APIC 0 enabled",
            "[INFO ] time: timer interrupt self-test passed",
            "[INFO ] console: serial input enabled",
            "[INFO ] sched: round-robin scheduler started; preemption self-test passed",
            "[INFO ] syscall: user mode and system call entry enabled",
            "[INFO ] virtio-blk: vda ready, 128 MiB; read test passed",
            "[INFO ] paging: audit passed (W^X holds)",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "shell-session",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            SUCCESS_MESSAGE,
            "[INFO ] vfs: RAM filesystem mounted at /",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            ("pwd", "\n/\n"),
            (
                "uname -a",
                concat!("\nOmit OS ", env!("CARGO_PKG_VERSION"), " x86_64\n"),
            ),
            ("mkdir /home", ""),
            ("mkdir -p /home/user/docs", ""),
            ("cd /home/user", ""),
            ("pwd", "\n/home/user\n"),
            ("echo hello from omit > greeting.txt", ""),
            ("cat greeting.txt", "\nhello from omit\n"),
            ("echo 'second  line' >> greeting.txt", ""),
            ("cat greeting.txt", "\nhello from omit\nsecond  line\n"),
            ("ls", "\ndocs\ngreeting.txt\n"),
            (
                "ls -l /home/user",
                "\nd          0 docs\n-         29 greeting.txt\n",
            ),
            ("cp greeting.txt docs", ""),
            ("mv docs/greeting.txt docs/copy.txt", ""),
            ("ls docs", "\ncopy.txt\n"),
            ("cat docs/copy.txt", "\nhello from omit\nsecond  line\n"),
            ("touch empty", ""),
            ("rm greeting.txt empty", ""),
            ("ls", "\ndocs\n"),
            (
                "cat greeting.txt",
                "\ncat: greeting.txt: No such file or directory\n",
            ),
            ("rm -r docs", ""),
            ("cd /", ""),
            ("ls /home/user", "ls /home/user\nomit:/# "),
            ("nosuchcommand", "\nnosuchcommand: command not found\n"),
            // User programs: each runs in ring 3 in its own address space.
            (
                "hello",
                "\nHello from user mode! I am process 1 and the system has been up for ",
            ),
            (
                "ls /bin",
                "\ncat\nclock\ncrash\ndesktop\necho\nechod\nfetch\nfileops\nhello\nhost\nipctest\nls\nmemtest\nnettest\npipetest\npop\nrandom\nrun\nscribble\nsleeper\nspin\nsyscheck\ntee\ntftp\nthreadtest\nwindll.exe\nwinfloat.exe\nwingui.exe\nwinhello.exe\nwinmissing.exe\nwinplugin.dll\nwinrun\nwinsample.dll\nwintest.exe\nwinthread.exe\n",
            ),
            // A program that misbehaves is stopped; the shell carries on.
            (
                "crash",
                "write(kernel address) -> Some(Fault)\nwrite(unmapped address) -> Some(Fault)\nsyscall 9999 -> Some(NoSuchSyscall)\nnow reading kernel memory directly...\n[process 2 (crash) terminated: #PF page fault accessing 0xffffffff80000000 at rip 0x4",
            ),
            ("echo still-alive", "\nstill-alive\n"),
            // Exit statuses reach the shell, and sleeping works.
            (
                "sleeper",
                "sleeper: tick 5\nsleeper: exited with status 7\n",
            ),
            // A program that never yields is preempted, visible and killable.
            ("spin &", "\n[4] spin\n"),
            ("ps", " spin\n"),
            ("kill 4", ""),
            ("sleeper", "sleeper: tick 5\n"),
            ("kill 4", "\nkill: 4: no such process\n"),
            ("free", "\nMemory: "),
            // Programs with arguments, files and standard input. The
            // built-ins `echo` and `cat` take precedence over the programs
            // of the same name, so those are run by path.
            ("/bin/echo  from   'user space'", "\nfrom user space\n"),
            ("echo file contents > /note", ""),
            ("/bin/cat /note /note", "\nfile contents\nfile contents\n"),
            ("cd /bin", ""),
            ("./cat ../note", "\nfile contents\n"),
            ("cd /", ""),
            (
                "/bin/cat /missing /note",
                "\ncat: /missing: No such file or directory\nfile contents\ncat: exited with status 1\n",
            ),
            ("/bin/cat /bin", "\ncat: /bin: Is a directory\n"),
            // Standard input: two lines typed, then Ctrl-D for end of input.
            (
                "tee /typed\rfirst line\rsecond line\r\x04",
                "first line\nfirst line\nsecond line\nsecond line\n",
            ),
            ("cat /typed", "\nfirst line\nsecond line\n"),
            // The program heap (the `alloc` crate in user space).
            (
                "memtest",
                "memtest: vector of 1000000 numbers ok\nmemtest: map of 5000 strings ok\nmemtest: 8 MiB block allocated and released 3 times ok\nomit:/# ",
            ),
            // Directory system calls, checked by the program itself.
            ("mkdir /scratch", ""),
            ("fileops /scratch", "\nfileops: all checks passed\n"),
            ("ls /scratch", "ls /scratch\nomit:/# "),
            // The user-space `ls` (by path; the built-in takes precedence).
            (
                "/bin/ls -l /typed /home",
                "\n-         23 /typed\n\n/home:\nd          0 user\n",
            ),
            ("/bin/ls /nope", "\nls: /nope: No such file or directory\n"),
            // Output redirection for programs.
            (
                "/bin/echo redirected > /out",
                "/bin/echo redirected > /out\nomit:/# ",
            ),
            ("/bin/echo appended >> /out", ""),
            ("cat /out", "\nredirected\nappended\n"),
            // A program starting other programs and collecting their status.
            (
                "run /bin/hello /bin/sleeper /bin/nothere",
                "run: /bin/hello exited with status 0\n",
            ),
            ("echo marker", "\nmarker\n"),
            // Line editing: the typo is erased with backspace before Enter.
            ("echo line-edix\x7fting", "\nline-editing\n"),
            // The CPU state after a system call return is what user mode
            // expects (segment selectors, flags, preserved registers),
            // including after being interrupted there.
            ("syscheck", "\nsyscheck: all checks passed\n"),
            ("random --check", "\nrandom: ok, from "),
            ("random 3", "\n"),
            // Pipes between processes: within one program, from another
            // program, and through a filter with more data than a pipe
            // holds.
            ("pipetest", "\npipetest: all checks passed\n"),
            // The date, from the (emulated, fixed) real-time clock.
            ("date", "\nTue 2026-11-10 12:0"),
            // Shared memory between two processes, channels, poll.
            ("ipctest", "\nipctest: all checks passed\n"),
            // Threads within a process: results through join, a mutex
            // under contention, the shared heap, the futex, and a process
            // that ends while two of its threads never would.
            ("threadtest", "\nthreadtest: all checks passed\nomit:/# "),
            ("ps", "PID"),
            // Pipelines and input redirection in the shell. `cat`, `ls`
            // and `echo` here are the programs in /bin: built-in commands
            // do not take part in pipelines.
            ("echo one two three > /words", ""),
            ("cat < /words", "\none two three\n"),
            ("echo piped text | cat", "\npiped text\n"),
            (
                "cat /words | tee /copy1 | tee /copy2 | cat",
                "\none two three\n",
            ),
            ("cat /copy1 /copy2", "\none two three\none two three\n"),
            ("ls /bin | tee /listing > /dev-null", ""),
            ("cat < /listing | cat | cat > /listing2", ""),
            (
                "cat /listing2",
                "\nwinrun\nwinsample.dll\nwintest.exe\nwinthread.exe\n",
            ),
            // A Windows program at the end of a pipeline and at its start.
            (
                "echo to windows | winhello.exe",
                "\nHello from a Windows executable!\r\n",
            ),
            (
                "winhello.exe | cat",
                "\nHello from a Windows executable!\r\n",
            ),
            // The status of a pipeline is that of its last command, and a
            // command that cannot be found stops the whole line.
            ("cat /words | sleeper", "sleeper: exited with status 7\n"),
            ("cat /words | nosuch", "\nnosuch: command not found\n"),
            ("cat < /nosuch", "\n/nosuch: No such file or directory\n"),
            (
                "pwd | cat",
                "\npwd: this built-in command cannot be used with '|' or '<'\n",
            ),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "display-and-keyboard",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            SUCCESS_MESSAGE,
            "[INFO ] display: framebuffer console",
            "[INFO ] console: PS/2 keyboard input enabled",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[("echo typed-on-serial", "\ntyped-on-serial\n")],
        reboot_script: &[],
        ends_with_reset: false,
        display: true,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    // The clocks measured against the ACPI power-management timer, as on
    // a machine whose legacy timer is switched off; here that timer
    // exists, so it serves as the check instead. The date then has to
    // advance at the right rate: `sleeper` sleeps and the uptime and
    // the clock must agree.
    BootTest {
        name: "calibrate-with-pm-timer",
        cmdline: "calibrate=pmtimer",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "tick 100 Hz; measured against the ACPI PM timer",
            "[INFO ] time: a second reference clock differs by ",
            "[INFO ] time: timer interrupt self-test passed",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            ("date", "\nTue 2026-11-10 12:0"),
            ("hello", "\nHello from user mode! I am process 1 "),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    // A machine without a serial port, like most current ones: the
    // kernel must come up on screen and keyboard alone. With no serial
    // log, the test can only see the screen and the exit status, which
    // is "success" only if the shell ran and `exit` was typed into it.
    BootTest {
        name: "no-serial-port",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[],
        must_not_contain: &[],
        script: &[],
        reboot_script: &[],
        ends_with_reset: false,
        display: true,
        desktop: false,
        qemu_args: &["-serial", "none"],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: true,
        network: None,
    },
    // The same with a USB keyboard on an xHCI controller, and the PS/2
    // devices switched off, so the typed line can only have come by USB.
    BootTest {
        name: "usb-keyboard",
        cmdline: "nops2",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            SUCCESS_MESSAGE,
            "[INFO ] console: PS/2 devices not used (nops2)",
            "[INFO ] xhci: controller started,",
            "[INFO ] usb: port 5: keyboard (0627:0001)",
            "[INFO ] usb: port 6: mouse (0627:0001)",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            ("echo typed-on-serial", "\ntyped-on-serial\n"),
            ("lsdrv", "native     xhci\n"),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: true,
        desktop: false,
        qemu_args: USB_INPUT,
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    // The same keyboard behind a hub, and a mouse behind a second hub
    // plugged into the first.
    BootTest {
        name: "usb-keyboard-behind-hubs",
        cmdline: "nops2",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            SUCCESS_MESSAGE,
            "[INFO ] usb: port 5.1: keyboard (0627:0001)",
            "[INFO ] usb: port 5.2.3: mouse (0627:0001)",
            "[INFO ] usb: port 5.2: hub with 8 ports",
            "[INFO ] usb: port 5: hub with 8 ports",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[("echo typed-on-serial", "\ntyped-on-serial\n")],
        reboot_script: &[],
        ends_with_reset: false,
        display: true,
        desktop: false,
        qemu_args: &[
            "-device",
            "qemu-xhci,id=xhci",
            "-device",
            "usb-hub,bus=xhci.0,port=1",
            "-device",
            "usb-kbd,bus=xhci.0,port=1.1",
            "-device",
            "usb-hub,bus=xhci.0,port=1.2",
            "-device",
            "usb-mouse,bus=xhci.0,port=1.2.3",
        ],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    // And the whole desktop scenario — pointer movement, clicks, drags,
    // typing — with USB mouse and keyboard only.
    BootTest {
        name: "desktop-usb-input",
        cmdline: "nops2",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] console: PS/2 devices not used (nops2)",
            "desktop: closed",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[("echo before-desktop", "\nbefore-desktop\n")],
        reboot_script: &[],
        ends_with_reset: false,
        display: true,
        desktop: true,
        qemu_args: USB_INPUT,
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "desktop",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] input: PS/2 mouse enabled",
            "desktop: closed",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[("echo before-desktop", "\nbefore-desktop\n")],
        reboot_script: &[],
        ends_with_reset: false,
        display: true,
        desktop: true,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "disk-persistence",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] virtio-blk: vda ready, 128 MiB; read test passed",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        // First boot: create a filesystem on the blank disk and fill it.
        script: &[
            ("lsblk", "\nvda      128 MiB\n"),
            (
                "lspci",
                " 1af4:1001 VirtIO block device (transitional) [native driver: virtio-blk]\n",
            ),
            ("mkdir /mnt", ""),
            ("mount vda /mnt", "\nmount: vda: not an ext2 filesystem\n"),
            ("mkfs vda", "\nmkfs: created an ext2 filesystem on vda\n"),
            ("mount vda /mnt", "mount vda /mnt\nomit:/# "),
            ("mount", "\nramfs on /\nvda on /mnt\n"),
            ("ls /mnt", "\nlost+found\n"),
            ("mkdir -p /mnt/home/user", ""),
            (
                "echo written before the reboot > /mnt/home/user/note.txt",
                "",
            ),
            ("echo second line >> /mnt/home/user/note.txt", ""),
            // A multi-block binary file, written by a program.
            ("cp /bin/hello /mnt/hello", ""),
            ("/bin/ls -l /bin > /mnt/listing.txt", ""),
            ("fileops /mnt", "\nfileops: all checks passed\n"),
            ("touch /mnt/doomed", ""),
            ("rm /mnt/doomed", ""),
            ("mkfs vda", "\nmkfs: vda: device is in use\n"),
            ("umount /mnt", "umount /mnt\nomit:/# "),
            ("ls /mnt", "ls /mnt\nomit:/# "),
        ],
        // Second boot, same disk: everything must still be there.
        reboot_script: &[
            ("mkdir /mnt", ""),
            ("mount vda /mnt", "mount vda /mnt\nomit:/# "),
            ("ls /mnt", "\nhello\nhome\nlisting.txt\nlost+found\n"),
            (
                "cat /mnt/home/user/note.txt",
                "\nwritten before the reboot\nsecond line\n",
            ),
            ("cat /mnt/listing.txt", " hello\n"),
            // A program loaded from the disk filesystem.
            ("/mnt/hello", "\nHello from user mode! I am process 1 "),
            ("umount /mnt", ""),
        ],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    // The installer: the system writes itself to the VirtIO disk, the
    // machine is restarted without the boot medium, and must come up from
    // the disk with the data partition in place.
    BootTest {
        name: "install-to-disk",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] block: vda has 2 partition(s)",
            "[INFO ] fs: data partition vda2 mounted at /data",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            ("lsblk", "\nvda      128 MiB\nomit:/# "),
            // Erasing a disk needs explicit confirmation.
            ("install vda", "install: usage: install DEVICE --erase\n"),
            (
                "install vda --erase",
                "\nvda2: data partition, 78 MiB, empty ext2 filesystem (mounted at /data when the installed system starts)\ninstalled on vda; remove the installation medium and restart\n",
            ),
            (
                "lsblk",
                "\nvda      128 MiB\nvda1      48 MiB\nvda2      78 MiB\n",
            ),
            // The new data partition can be used at once; leave something
            // on it for the installed system to find.
            ("mkdir /data", ""),
            ("mount vda2 /data", "mount vda2 /data\nomit:/# "),
            ("echo written before the restart > /data/note", ""),
            ("cp /bin/hello /data/hello", ""),
            // A disk with a mounted filesystem is not installed over.
            (
                "install vda --erase",
                "install: vda: a filesystem on this disk is mounted; unmount it first\n",
            ),
            ("umount /data", "umount /data\nomit:/# "),
            // Nor is a partition a place to install to.
            (
                "install vda2 --erase",
                "install: vda2: this is a partition; install onto the whole disk\n",
            ),
        ],
        // Second boot: from the VirtIO disk alone.
        reboot_script: &[
            ("mount", "\nramfs on /\nvda2 on /data\n"),
            ("cat /data/note", "\nwritten before the restart\n"),
            ("/data/hello", "\nHello from user mode! I am process 1 "),
            // The installed system is complete: programs, Windows
            // programs, and the files the installer itself needs.
            ("winhello.exe", "\nHello from a Windows executable!\r\n"),
            ("ls /boot", "\nBOOTX64.EFI\nKERNEL.ELF\n"),
            ("echo and after > /data/second", ""),
            ("umount /data", "umount /data\nomit:/# "),
        ],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: true,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    // The same on an NVMe disk, the kind most current machines have: the
    // native NVMe driver, installation, and booting from the result with
    // an empty VirtIO disk still attached.
    BootTest {
        name: "install-to-nvme",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] nvme: nvme0n1 ready, 128 MiB, model \"QEMU NVMe Ctrl\"; read test passed",
            "[INFO ] block: nvme0n1 has 2 partition(s)",
            "[INFO ] fs: data partition nvme0n1p2 mounted at /data",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            ("lsblk", "\nnvme0n1   128 MiB\n"),
            ("lsdrv", "native     nvme\n"),
            (
                "lspci",
                " 1b36:0010 QEMU NVMe controller [native driver: nvme]\n",
            ),
            (
                "install nvme0n1 --erase",
                "\nnvme0n1p2: data partition, 78 MiB, empty ext2 filesystem (mounted at /data when the installed system starts)\ninstalled on nvme0n1; remove the installation medium and restart\n",
            ),
            ("lsblk", "\nnvme0n1p1    48 MiB\nnvme0n1p2    78 MiB\n"),
            ("mkdir /data", ""),
            ("mount nvme0n1p2 /data", "mount nvme0n1p2 /data\nomit:/# "),
            ("echo kept on the NVMe disk > /data/note", ""),
            // A file larger than one command can carry (4 KiB), so that
            // reads and writes are split.
            ("cp /bin/desktop /data/desktop", ""),
            ("umount /data", "umount /data\nomit:/# "),
        ],
        // Second boot: from the NVMe disk alone.
        reboot_script: &[
            ("mount", "\nramfs on /\nnvme0n1p2 on /data\n"),
            ("cat /data/note", "\nkept on the NVMe disk\n"),
            ("cmp /bin/desktop /data/desktop", ""),
            ("winhello.exe", "\nHello from a Windows executable!\r\n"),
            ("umount /data", "umount /data\nomit:/# "),
        ],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: true,
        nvme: true,
        sata: false,
        no_serial: false,
        network: None,
    },
    // And on a SATA disk, the kind virtual machines offer by default.
    // (When the test boots from a disk image, that image is a SATA disk
    // too, on port 0; the target is on port 2 either way.)
    BootTest {
        name: "install-to-sata",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] ahci: sdc ready, 128 MiB, model \"QEMU HARDDISK\"",
            "[INFO ] block: sdc has 2 partition(s)",
            "[INFO ] fs: data partition sdc2 mounted at /data",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            ("lsblk", "\nsdc      128 MiB\n"),
            ("lsdrv", "native     ahci\n"),
            (
                "lspci",
                " 8086:2922 Intel ICH9 SATA controller (AHCI) [native driver: ahci]\n",
            ),
            (
                "install sdc --erase",
                "\nsdc2: data partition, 78 MiB, empty ext2 filesystem (mounted at /data when the installed system starts)\ninstalled on sdc; remove the installation medium and restart\n",
            ),
            (
                "lsblk",
                "\nsdc      128 MiB\nsdc1      48 MiB\nsdc2      78 MiB\n",
            ),
            ("mkdir /data", ""),
            ("mount sdc2 /data", "mount sdc2 /data\nomit:/# "),
            ("echo kept on the SATA disk > /data/note", ""),
            ("cp /bin/desktop /data/desktop", ""),
            ("umount /data", "umount /data\nomit:/# "),
        ],
        // Second boot: from the SATA disk alone.
        reboot_script: &[
            ("mount", "\nramfs on /\nsdc2 on /data\n"),
            ("cat /data/note", "\nkept on the SATA disk\n"),
            ("cmp /bin/desktop /data/desktop", ""),
            ("winhello.exe", "\nHello from a Windows executable!\r\n"),
            ("umount /data", "umount /data\nomit:/# "),
        ],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: true,
        nvme: false,
        sata: true,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "windows-programs",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "winfloat 1: registers intact, H(1000) = 7.485470",
            "[INFO ] shell exited",
        ],
        must_not_contain: &[
            "KERNEL PANIC",
            "KERNEL EXCEPTION",
            "[ERROR]",
            "[WARN ]",
            "CORRUPTED",
            "FAILED",
        ],
        script: &[
            // A Windows executable is recognised and run by name.
            ("winhello.exe", "\nHello from a Windows executable!\r\n"),
            // Arguments arrive as a Windows command line; the exit code
            // returned from the entry point reaches the shell.
            (
                "wintest.exe one 'two words' three",
                "wintest: command line: [/bin/wintest.exe one \"two words\" three]\r\n",
            ),
            ("echo marker-1", "\nmarker-1\n"),
            // The whole run, into a file, to check every line of it.
            (
                "wintest.exe > /wintest.log",
                "wintest.exe: exited with status 42\n",
            ),
            (
                "cat /wintest.log",
                "wintest: relocated data works\r\nwintest: image loaded at a relocated base: true\r\nwintest: heap and virtual memory ok\r\nwintest: files and directories ok\r\nwintest: wide characters ok\r\nwintest: child process started\r\nHello from a Windows executable!\r\nwintest: processes ok\r\nwintest: date 2026-11-10, day 2 of the week\r\nwintest: time ok\r\nwintest: all 94 checks passed\r\n",
            ),
            // The directory the test created and removed is gone.
            ("ls /", "ls /\nbin\nboot\nwintest.log\nomit:/# "),
            // Libraries: one imported by the program, one loaded on
            // request that itself uses the first.
            (
                "windll.exe",
                "[winsample.dll] printed by the library through its own imports\r\nwindll: program file: C:\\bin\\windll.exe\r\nwindll: all 33 checks passed\r\n",
            ),
            // Threads in a Windows program: critical sections, events,
            // thread-local storage, exit codes; a thread still running
            // when the program returns ends with it.
            (
                "winthread.exe",
                "winthread: 4 threads counted to 20000\r\nwinthread: events ok\r\nwinthread: mutexes and semaphores ok\r\nwinthread: all",
            ),
            ("ps", "PID"),
            // A library is not a program.
            (
                "winrun /bin/winsample.dll",
                "winrun: /bin/winsample.dll: this is a library, not a program\n",
            ),
            // Missing functions are reported before the program starts.
            (
                "winmissing.exe",
                "winrun: /bin/winmissing.exe: needs kernel32.dll!CreateNamedPipeA, which is not available\nwinrun: /bin/winmissing.exe: needs advapi32.dll (library not found), which is not available\nwinrun: /bin/winmissing.exe: needs user32.dll!CloseClipboard, which is not available\nwinrun: /bin/winmissing.exe: 3 of the things it needs are missing\nwinmissing.exe: exited with status 127\n",
            ),
            // Files that are not 64-bit Windows programs.
            ("echo MZ-but-not-really > /fake.exe", ""),
            (
                "/fake.exe",
                "winrun: /fake.exe: not a Windows executable (no PE signature)\nfake.exe: exited with status 126\n",
            ),
            ("winrun", "usage: winrun PROGRAM.EXE [ARGUMENT...]\n"),
            (
                "winrun --list",
                "\nKERNEL32.dll!WriteFile\nUSER32.dll!BeginPaint\n",
            ),
            // A graphical program without a desktop to show its window
            // on: creating the window fails, and the program says so.
            (
                "wingui.exe",
                "\nwingui: cannot create a window (error 50)\r\n",
            ),
            (
                "winrun /bin/hello",
                "winrun: /bin/hello: not an executable (no MZ signature)\n",
            ),
            // Two Windows programs at once: each keeps values in SSE
            // registers and checks that the other's never show up.
            // (The first one's report is checked in `must_contain`.)
            // The background one runs for a shorter time, so that it has
            // reported before the foreground one returns.
            ("winfloat.exe 1 800 &", "] winfloat.exe\n"),
            (
                "winfloat.exe 2 2000",
                "winfloat 2: registers intact, H(1000) = 7.485470\r\n",
            ),
            // A program started by another program (the spawn system
            // call) can be a Windows executable too.
            (
                "run /bin/winhello.exe",
                "\nHello from a Windows executable!\r\n",
            ),
            ("ps", "PID"),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "linux-driver-watchdog",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] linux: 00:03.0: initialized. heartbeat=30 sec (nowayout=0)",
            "[INFO ] linuxkpi: 1 device(s) bound to Linux drivers",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            (
                "lspci",
                "\n00:03.0 8086:25ab Intel 6300ESB watchdog timer [Linux driver: i6300esb]\n",
            ),
            (
                "lsdrv",
                "\n00:02.0  native     virtio-blk\n00:1f.2  native     ahci\n00:03.0  linux-kpi  i6300ESB timer\n",
            ),
            (
                "watchdog",
                "\ni6300ESB timer: stopped, timeout 30 s (range 1-2046 s)\n",
            ),
            (
                "watchdog timeout 9999",
                "\nwatchdog: timeout outside the range",
            ),
            ("watchdog timeout 6", "\nwatchdog: timeout set to 6 s\n"),
            ("watchdog start", "\nwatchdog: started\n"),
            ("watchdog", "\ni6300ESB timer: running, timeout 6 s"),
            // Stay alive past the 6 s timeout by sending keep-alives: each
            // `sleeper` run takes at least half a second, so fourteen rounds
            // take longer than the timeout, while a single round stays well
            // below it even in slow, unoptimised builds.
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("watchdog ping", "\nwatchdog: keep-alive sent\n"),
            // Stopped, the machine must survive without keep-alives, even
            // with the shortest timeout programmed.
            ("watchdog stop", "\nwatchdog: stopped\n"),
            ("watchdog timeout 1", "\nwatchdog: timeout set to 1 s\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("sleeper", "sleeper: tick 5\n"),
            ("sleeper", "sleeper: tick 5\n"),
            // Unbinding calls the driver's remove function.
            ("unbind 00:03.0", "unbind 00:03.0\nomit:/# "),
            ("watchdog", "\nwatchdog: no watchdog device\n"),
            ("lsdrv", "ahci\nomit:/# "),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "linux-driver-watchdog-expiry",
        cmdline: "",
        // The watchdog resets the machine; with -no-reboot QEMU then exits
        // normally instead of through the kernel's debug-exit device.
        expected_exit: 0,
        must_contain: &["watchdog: started"],
        must_not_contain: &[
            "KERNEL PANIC",
            "KERNEL EXCEPTION",
            "[ERROR]",
            "[INFO ] shell exited",
        ],
        script: &[
            ("watchdog timeout 1", "\nwatchdog: timeout set to 1 s\n"),
            // Started and never pinged: the hardware must reset the machine.
            ("watchdog start", "\nwatchdog: started\n"),
        ],
        reboot_script: &[],
        ends_with_reset: true,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "linux-driver-pvpanic",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &["[INFO ] shell exited"],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            (
                "lspci",
                " 1b36:0011 QEMU pvpanic device [Linux driver: pvpanic-pci]\n",
            ),
            ("lsdrv", "00:04.0  linux-kpi  pvpanic-pci\n"),
            // The attributes the driver declares; on Linux they would be
            // files in sysfs.
            (
                "devattr 00:04.0",
                "\ncapability       read-only\nevents           read/write\n",
            ),
            // The device (as emulated by QEMU 8) reports "panicked" and
            // "crash loaded" events: bits 0 and 1.
            ("devattr 00:04.0 capability", "\n3\n"),
            ("devattr 00:04.0 events", "\n3\n"),
            // Writing goes through the driver's own parsing and checks.
            (
                "devattr 00:04.0 events 1",
                "devattr 00:04.0 events 1\nomit:/# ",
            ),
            ("devattr 00:04.0 events", "\n1\n"),
            (
                "devattr 00:04.0 events 8",
                "devattr: 00:04.0: the driver rejected the value (error 22)\n",
            ),
            (
                "devattr 00:04.0 events junk",
                "devattr: 00:04.0: the driver rejected the value (error 22)\n",
            ),
            (
                "devattr 00:04.0 capability 1",
                "devattr: 00:04.0: the attribute is read-only\n",
            ),
            ("devattr 00:04.0 events", "\n1\n"),
            ("devattr 00:03.0", "devattr 00:03.0\nomit:/# "),
            (
                "devattr 00:02.0",
                "devattr: 00:02.0: no Linux driver is bound to that device\n",
            ),
            // Unbinding runs the clean-up action the driver registered.
            ("unbind 00:04.0", "unbind 00:04.0\nomit:/# "),
            (
                "devattr 00:04.0",
                "devattr: 00:04.0: no Linux driver is bound to that device\n",
            ),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &["-device", "pvpanic-pci"],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    // A VirtIO network card on QEMU's built-in user-mode network, which
    // has a DHCP server and a gateway at 10.0.2.2 that answers echo
    // requests itself. The test only talks to that gateway, so nothing
    // leaves the machine it runs on.
    BootTest {
        name: "network",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] virtio-net: 52:54:00:12:34:56 is eth0",
            "[INFO ] net: eth0: address 10.0.2.15/24, router 10.0.2.2, name server 10.0.2.3 (DHCP)",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            ("lsdrv", "native     virtio-net\n"),
            (
                "lspci",
                " 1af4:1000 VirtIO network device (transitional) [native driver: virtio-net]\n",
            ),
            (
                "ifconfig",
                "\neth0: virtio-net, hardware address 52:54:00:12:34:56\n    address 10.0.2.15/24, router 10.0.2.2, name server 10.0.2.3\n    frames: ",
            ),
            // The gateway: found by ARP, then asked three times.
            ("ping 10.0.2.2", "\n3 sent, 3 received\n"),
            (
                "ifconfig",
                "\n    neighbour 10.0.2.2 is at 52:55:0a:00:02:02\n",
            ),
            (
                "ping 10.0.2.2 1",
                "\n32 bytes from 10.0.2.2: seq=1 ttl=255 time=",
            ),
            // An address on the local network that nobody has.
            (
                "ping 10.0.2.99 1",
                "\nseq=1: unreachable: nobody on the local network answers for it\n1 sent, 0 received\nping: no answer from 10.0.2.99\n",
            ),
            (
                "ping 10.0.2.15 1",
                "\nseq=1: that is this machine's own address, which cannot be pinged yet\n",
            ),
            // The disk on the other VirtIO device still works.
            ("lsblk", "\nvda"),
            // UDP sockets, from a program: files fetched from QEMU's
            // TFTP server. One of 300 bytes (a single short block), one
            // of exactly two blocks (which ends with an empty third),
            // and a program of several hundred blocks, compared with
            // the copy the system was started with.
            (
                "tftp 10.0.2.2 short.txt /short",
                "\nshort.txt: 300 bytes from 10.0.2.2, stored as /short\n",
            ),
            (
                "tftp 10.0.2.2 exact.bin",
                "\nexact.bin: 1024 bytes from 10.0.2.2, stored as exact.bin\n",
            ),
            (
                "tftp 10.0.2.2 desktop /desktop.copy",
                " bytes from 10.0.2.2, stored as /desktop.copy\n",
            ),
            (
                "cmp /bin/desktop /desktop.copy",
                "cmp /bin/desktop /desktop.copy\nomit:/# ",
            ),
            (
                "tftp 10.0.2.2 hello /hello.copy",
                " bytes from 10.0.2.2, stored as /hello.copy\n",
            ),
            ("/hello.copy", "\nHello from user mode! I am process "),
            // A file the server does not have: its refusal is shown and
            // nothing is left behind.
            (
                "tftp 10.0.2.2 missing.txt /missing",
                "\ntftp: missing.txt: the server says: ",
            ),
            ("cat /missing", "No such file or directory"),
            // A server that is not there.
            (
                "tftp 10.0.2.99 short.txt /unreachable",
                "\ntftp: short.txt: the server cannot be reached\n",
            ),
            // Names, asked of a name server run by the test on the host
            // (the gateway address leads to the host's own loopback).
            (
                "host test.omit.example 10.0.2.2:15353",
                "\ntest.omit.example has address 192.0.2.7\n",
            ),
            (
                "host missing.omit.example 10.0.2.2:15353",
                "\nhost: missing.omit.example: no such name\n",
            ),
            // Nobody listens on that port: three questions, no answer.
            (
                "host test.omit.example 10.0.2.2:15354",
                "\nhost: test.omit.example: no answer from 10.0.2.2\n",
            ),
            ("host 10.0.2.2", "\n10.0.2.2 is an address already\n"),
            // The socket system calls themselves, including what they
            // must refuse.
            (
                "nettest 10.0.2.2 15353 18080",
                "\nnettest: all checks passed\n",
            ),
            // TCP, from a program: files fetched over HTTP from a web
            // server the test runs on the host.
            (
                "fetch http://10.0.2.2:18080/hello.txt",
                "\nHello from the test server\nomit:/# ",
            ),
            (
                "fetch http://10.0.2.2:18080/desktop /desktop.http",
                " bytes, stored as /desktop.http\n",
            ),
            (
                "cmp /bin/desktop /desktop.http",
                "cmp /bin/desktop /desktop.http\nomit:/# ",
            ),
            // A body whose end is the end of the connection.
            (
                "fetch http://10.0.2.2:18080/nolength",
                "\nno length given\nomit:/# ",
            ),
            // What a server can say instead, and what can go wrong.
            (
                "fetch http://10.0.2.2:18080/missing /missing",
                "\nfetch: the server says: 404 Not Found\n",
            ),
            (
                "fetch http://10.0.2.2:18080/moved",
                "(301 Moved Permanently); redirects are not followed\n",
            ),
            (
                "fetch http://10.0.2.2:18080/short /short.http",
                "\nfetch: the connection ended after 9 of 100 bytes\n",
            ),
            ("cat /short.http", "/short.http: No such file or directory"),
            (
                "fetch http://10.0.2.2:18081/",
                "\nfetch: 10.0.2.2: nothing listens on that port\n",
            ),
            (
                "fetch http://10.0.2.99/",
                "\nfetch: 10.0.2.99: the host cannot be reached\n",
            ),
            (
                "fetch ftp://example.org/",
                "only http:// and https:// addresses can be fetched\n",
            ),
            // The same server behind TLS. Its certificate is signed by
            // an authority made for this run, whose certificate the
            // guest fetches first (over TFTP, which protects nothing:
            // good enough for a test).
            (
                "tftp 10.0.2.2 ca.pem /ca.pem",
                " bytes from 10.0.2.2, stored as /ca.pem\n",
            ),
            (
                "fetch --trust /ca.pem https://10.0.2.2:18443/hello.txt",
                "\nHello from the test server\nomit:/# ",
            ),
            (
                "fetch --trust /ca.pem https://10.0.2.2:18443/desktop /desktop.https",
                " bytes, stored as /desktop.https\n",
            ),
            (
                "cmp /bin/desktop /desktop.https",
                "cmp /bin/desktop /desktop.https\nomit:/# ",
            ),
            (
                "fetch --trust /ca.pem https://10.0.2.2:18443/nolength",
                "\nno length given\nomit:/# ",
            ),
            // Without the authority, the server is a stranger.
            (
                "fetch https://10.0.2.2:18443/hello.txt",
                "\nfetch: 10.0.2.2: the server's certificate is not signed by an authority this system trusts\n",
            ),
            // A file larger than any one stretch of memory the kernel
            // can allocate, fetched both ways; the two copies must be
            // the same.
            (
                "fetch http://10.0.2.2:18080/big /big.http",
                ": 6291456 bytes, stored as /big.http\n",
            ),
            (
                "fetch --trust /ca.pem https://10.0.2.2:18443/big /big.https",
                ": 6291456 bytes, stored as /big.https\n",
            ),
            (
                "cmp /big.http /big.https",
                "cmp /big.http /big.https\nomit:/# ",
            ),
            ("rm /big.http", ""),
            ("rm /big.https", ""),
            // A connection that stops without the server saying it has
            // finished.
            (
                "fetch --trust /ca.pem https://10.0.2.2:18443/short /short.https",
                "\nfetch: 10.0.2.2: the connection was cut off\n",
            ),
            (
                "cat /short.https",
                "/short.https: No such file or directory",
            ),
            // What is not a certificate is not trusted.
            (
                "fetch --trust /bin/hello https://10.0.2.2:18443/hello.txt",
                "not a certificate (PEM or DER)\n",
            ),
            // A server that does not speak TLS: the test's plain one
            // makes nothing of the greeting and hangs up.
            (
                "fetch https://10.0.2.2:18080/hello.txt",
                "\nfetch: 10.0.2.2: the connection was cut off\n",
            ),
            // The other direction: a server in the guest, reached from
            // the host through a forwarded port. The host sends 20 000
            // bytes and checks that they come back unchanged (see
            // `start_test_echo_client`).
            ("echod 7 1", "\nechod: echoed 20000 bytes for 10.0.2.2\n"),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: Some("virtio-net-pci"),
    },
    // The same network through the other driver: an Intel card instead
    // of the VirtIO one. A shorter script: what differs is the driver,
    // so what matters is frames in both directions, many of them, and
    // large ones.
    BootTest {
        name: "network-e1000",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] e1000: 52:54:00:12:34:56 is eth0",
            "[INFO ] net: eth0: address 10.0.2.15/24, router 10.0.2.2, name server 10.0.2.3 (DHCP)",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            ("lsdrv", "native     e1000\n"),
            (
                "lspci",
                " 8086:100e Intel 82540EM Gigabit Ethernet [native driver: e1000]\n",
            ),
            (
                "ifconfig",
                "\neth0: e1000, hardware address 52:54:00:12:34:56\n    address 10.0.2.15/24, router 10.0.2.2, name server 10.0.2.3\n    frames: ",
            ),
            ("ping 10.0.2.2", "\n3 sent, 3 received\n"),
            (
                "ping 10.0.2.99 1",
                "\n1 sent, 0 received\nping: no answer from 10.0.2.99\n",
            ),
            // A megabyte in over a thousand frames, several times round
            // the receive ring; and the same again inside TLS.
            (
                "tftp 10.0.2.2 desktop /desktop.copy",
                " bytes from 10.0.2.2, stored as /desktop.copy\n",
            ),
            (
                "cmp /bin/desktop /desktop.copy",
                "cmp /bin/desktop /desktop.copy\nomit:/# ",
            ),
            (
                "fetch http://10.0.2.2:18080/desktop /desktop.http",
                " bytes, stored as /desktop.http\n",
            ),
            (
                "cmp /bin/desktop /desktop.http",
                "cmp /bin/desktop /desktop.http\nomit:/# ",
            ),
            (
                "tftp 10.0.2.2 ca.pem /ca.pem",
                " bytes from 10.0.2.2, stored as /ca.pem\n",
            ),
            (
                "fetch --trust /ca.pem https://10.0.2.2:18443/desktop /desktop.https",
                " bytes, stored as /desktop.https\n",
            ),
            (
                "cmp /bin/desktop /desktop.https",
                "cmp /bin/desktop /desktop.https\nomit:/# ",
            ),
            (
                "host test.omit.example 10.0.2.2:15353",
                "\ntest.omit.example has address 192.0.2.7\n",
            ),
            // The other direction: the host sends 20 000 bytes through
            // an echo server in the guest.
            ("echod 7 1", "\nechod: echoed 20000 bytes for 10.0.2.2\n"),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: Some("e1000"),
    },
    // The driver's other card: the 82545EM.
    BootTest {
        name: "network-e1000-82545em",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
            "[INFO ] e1000: 52:54:00:12:34:56 is eth0",
            "[INFO ] net: eth0: address 10.0.2.15/24, router 10.0.2.2, name server 10.0.2.3 (DHCP)",
            "[INFO ] shell exited",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[
            (
                "lspci",
                " 8086:100f Intel 82545EM Gigabit Ethernet [native driver: e1000]\n",
            ),
            ("ping 10.0.2.2", "\n3 sent, 3 received\n"),
            (
                "fetch http://10.0.2.2:18080/desktop /desktop.http",
                " bytes, stored as /desktop.http\n",
            ),
            (
                "cmp /bin/desktop /desktop.http",
                "cmp /bin/desktop /desktop.http\nomit:/# ",
            ),
            ("echod 7 1", "\nechod: echoed 20000 bytes for 10.0.2.2\n"),
        ],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: Some("e1000-82545em"),
    },
    // With the pvpanic device and its Linux driver, a kernel panic is
    // reported to the hypervisor. QEMU is told to exit with status 1 when
    // that happens, which is distinct from the status of the kernel's own
    // debug-exit device (see "panic-diagnostics", which has no pvpanic
    // device) — so status 1 proves the driver's panic notifier ran and
    // wrote to the device.
    BootTest {
        name: "linux-driver-pvpanic-notifies-host",
        cmdline: "fault=panic",
        expected_exit: 1,
        must_contain: &[
            "[INFO ] linuxkpi: 2 device(s) bound to Linux drivers",
            "==== KERNEL PANIC ====",
        ],
        must_not_contain: &[],
        script: &[],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &["-device", "pvpanic-pci", "-action", "panic=exit-failure"],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "panic-diagnostics",
        cmdline: "fault=panic",
        expected_exit: QEMU_EXIT_FAILURE,
        must_contain: &[
            SUCCESS_MESSAGE,
            "==== KERNEL PANIC ====",
            "message: fault injection: deliberate panic requested",
            "location: kernel/src/main.rs:",
            "  #0 0xffffffff8",
        ],
        must_not_contain: &["KERNEL EXCEPTION"],
        script: &[],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "page-fault-diagnostics",
        cmdline: "fault=pagefault",
        expected_exit: QEMU_EXIT_FAILURE,
        must_contain: &[
            "==== KERNEL EXCEPTION ====",
            "exception: #PF page fault (vector 14, error code 0x0)",
            "fault address: 0x0000000000001000 (non-present page on read in kernel mode)",
        ],
        must_not_contain: &["KERNEL PANIC"],
        script: &[],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "use-after-unmap-faults",
        cmdline: "fault=unmapped",
        expected_exit: QEMU_EXIT_FAILURE,
        must_contain: &[
            "==== KERNEL EXCEPTION ====",
            "exception: #PF page fault (vector 14, error code 0x0)",
            "fault address: 0xffffd00000000000 (non-present page on read in kernel mode)",
        ],
        must_not_contain: &["KERNEL PANIC", "[ERROR]"],
        script: &[],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
    BootTest {
        name: "stack-overflow-double-fault",
        cmdline: "fault=stackoverflow",
        expected_exit: QEMU_EXIT_FAILURE,
        must_contain: &[
            "==== KERNEL EXCEPTION ====",
            "exception: #DF double fault (vector 8",
        ],
        must_not_contain: &["KERNEL PANIC"],
        script: &[],
        reboot_script: &[],
        ends_with_reset: false,
        display: false,
        desktop: false,
        qemu_args: &[],
        boot_installed: false,
        nvme: false,
        sata: false,
        no_serial: false,
        network: None,
    },
];

/// Outcome of running QEMU to completion.
struct QemuRun {
    /// Exit status, or `None` if QEMU was killed after the timeout.
    exit: Option<i32>,
    serial: String,
    elapsed: Duration,
}

/// Where to reach QEMU's monitor and where to put the screenshot.
struct DisplayCheck {
    monitor: PathBuf,
    screenshot: PathBuf,
    /// Also run the graphical desktop and operate it with the mouse.
    desktop: bool,
}

/// An xHCI controller with a USB keyboard and a USB mouse.
const USB_INPUT: &[&str] = &[
    "-device",
    "qemu-xhci,id=xhci",
    "-device",
    "usb-kbd,bus=xhci.0",
    "-device",
    "usb-mouse,bus=xhci.0",
];

/// QEMU key names that type `echo kbd-ok` followed by Enter.
const KEYBOARD_TEST_KEYS: &[&str] = &[
    "e", "c", "h", "o", "spc", "k", "b", "d", "minus", "o", "k", "ret",
];
const KEYBOARD_TEST_OUTPUT: &str = "\nkbd-ok\n";

/// Colours the kernel's framebuffer console uses; must match
/// `kernel/src/fbcon.rs`.
const SCREEN_BACKGROUND: [u8; 3] = [0x10, 0x14, 0x1C];
const SCREEN_FOREGROUND: [u8; 3] = [0xD0, 0xD4, 0xDC];
const SCREEN_STATUS_BACKGROUND: [u8; 3] = [0x2F, 0x6F, 0xDE];
const SCREEN_STATUS_FOREGROUND: [u8; 3] = [0xFF, 0xFF, 0xFF];
/// Height of one text row in pixels.
const SCREEN_ROW_HEIGHT: usize = 16;

/// Sends one command to QEMU's human monitor.
#[cfg(unix)]
fn monitor_command(socket: &Path, command: &str) -> Result<()> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(format!("{command}\n").as_bytes())?;
    // Give QEMU time to read and act before the connection closes.
    thread::sleep(Duration::from_millis(60));
    Ok(())
}

#[cfg(not(unix))]
fn monitor_command(_socket: &Path, _command: &str) -> Result<()> {
    Err("the QEMU monitor is only supported on Unix hosts".into())
}

/// Presses `keys` one after another on the emulated keyboard.
fn type_on_keyboard(monitor: &Path, keys: &[&str]) -> Result<()> {
    for key in keys {
        // Hold each key for 20 ms.
        monitor_command(monitor, &format!("sendkey {key} 20"))?;
    }
    Ok(())
}

/// A decoded binary PPM image.
struct Screenshot {
    width: usize,
    height: usize,
    /// RGB triples, row by row.
    pixels: Vec<u8>,
}

impl Screenshot {
    fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let at = (y * self.width + x) * 3;
        [self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]]
    }

    /// Counts pixels of exactly `colour` in rows `rows`.
    fn count(&self, rows: std::ops::Range<usize>, colour: [u8; 3]) -> usize {
        rows.flat_map(|y| (0..self.width).map(move |x| (x, y)))
            .filter(|&(x, y)| self.pixel(x, y) == colour)
            .count()
    }
}

/// Parses a binary PPM (`P6`, 8 bits per channel) as written by QEMU.
fn parse_ppm(data: &[u8]) -> Result<Screenshot> {
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while data.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let start = at;
        while data.get(at).is_some_and(|b| !b.is_ascii_whitespace()) {
            at += 1;
        }
        if start == at {
            return Err("truncated PPM header".into());
        }
        fields.push(std::str::from_utf8(&data[start..at])?.to_owned());
    }
    if fields[0] != "P6" || fields[3] != "255" {
        return Err("not an 8-bit binary PPM".into());
    }
    let (width, height): (usize, usize) = (fields[1].parse()?, fields[2].parse()?);
    // Exactly one whitespace byte separates the header from the pixels.
    let pixels = data
        .get(at + 1..at + 1 + width * height * 3)
        .ok_or("truncated PPM data")?
        .to_vec();
    Ok(Screenshot {
        width,
        height,
        pixels,
    })
}

/// Asks QEMU for a screenshot and waits for the file to be complete.
fn take_screenshot(monitor: &Path, path: &Path) -> Result<Screenshot> {
    let _ = fs::remove_file(path);
    monitor_command(monitor, &format!("screendump {}", path.display()))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(data) = fs::read(path)
            && let Ok(image) = parse_ppm(&data)
        {
            return Ok(image);
        }
        if Instant::now() >= deadline {
            return Err("QEMU did not produce a screenshot".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn rgb(colour: omit_gfx::Colour) -> [u8; 3] {
    [(colour >> 16) as u8, (colour >> 8) as u8, colour as u8]
}

/// Whether `image` contains `text` exactly as the desktop draws it in a
/// window (same font, same colours), anywhere on the screen.
fn screen_shows(image: &Screenshot, text: &str) -> bool {
    let theme = omit_gfx::theme::Theme::default();
    screen_shows_in(image, text, theme.text, theme.window)
}

/// Whether `image` contains `text` as the desktop writes it in `font`,
/// in `colour` on a plain `background`, anywhere on the screen.
fn screen_writes(
    image: &Screenshot,
    text: &str,
    font: omit_gfx::font::Font,
    colour: omit_gfx::Colour,
    background: omit_gfx::Colour,
) -> bool {
    use omit_gfx::surface::Surface;
    // A margin, since letters may reach a little beyond their line.
    let margin = 2;
    let (width, height) = (
        (font.width(text) + 2 * margin) as usize,
        (font.height() + 2 * margin) as usize,
    );
    let mut rendered = Surface::new(width, height, background);
    rendered.canvas().write(margin, margin, text, font, colour);
    find_on_screen(image, &rendered)
}

/// Whether `image` contains the picture `rendered` anywhere.
fn find_on_screen(image: &Screenshot, rendered: &omit_gfx::surface::Surface) -> bool {
    let (width, height) = (rendered.width(), rendered.height());
    let pattern: Vec<[u8; 3]> = rendered.pixels().iter().map(|&p| rgb(p)).collect();
    if image.width < width || image.height < height {
        return false;
    }
    (0..=image.height - height).any(|top| {
        (0..=image.width - width).any(|left| {
            // The first row decides almost always; look at it first.
            pattern.iter().enumerate().all(|(index, &colour)| {
                image.pixel(left + index % width, top + index / width) == colour
            })
        })
    })
}

/// Whether `image` contains `text` drawn in `colour` on `background`.
fn screen_shows_in(
    image: &Screenshot,
    text: &str,
    colour: omit_gfx::Colour,
    background: omit_gfx::Colour,
) -> bool {
    use omit_gfx::surface::{Surface, TEXT_HEIGHT, text_width};
    let (width, height) = (text_width(text) as usize, TEXT_HEIGHT as usize);
    let mut rendered = Surface::new(width, height, background);
    rendered.canvas().text(0, 0, text, colour);
    find_on_screen(image, &rendered)
}

/// Takes screenshots until `accept` is satisfied or `timeout` passes.
fn wait_for_screen(
    check: &DisplayCheck,
    what: &str,
    timeout: Duration,
    accept: impl Fn(&Screenshot) -> bool,
) -> Result<Screenshot> {
    let deadline = Instant::now() + timeout;
    loop {
        let image = take_screenshot(&check.monitor, &check.screenshot)?;
        if accept(&image) {
            return Ok(image);
        }
        if Instant::now() >= deadline {
            return Err(format!("the screen never showed {what}").into());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// The emulated mouse. QEMU's PS/2 mouse reports relative movement, so
/// the position is tracked here after first driving the pointer into the
/// top-left corner.
struct Mouse<'a> {
    monitor: &'a Path,
    x: i32,
    y: i32,
}

impl<'a> Mouse<'a> {
    fn new(monitor: &'a Path) -> Result<Self> {
        // Far more than any screen is wide or high.
        for _ in 0..20 {
            monitor_command(monitor, "mouse_move -250 -250")?;
        }
        Ok(Self {
            monitor,
            x: 0,
            y: 0,
        })
    }

    fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        while (self.x, self.y) != (x, y) {
            // Small steps: one PS/2 packet carries at most 255 units.
            let dx = (x - self.x).clamp(-100, 100);
            let dy = (y - self.y).clamp(-100, 100);
            monitor_command(self.monitor, &format!("mouse_move {dx} {dy}"))?;
            self.x += dx;
            self.y += dy;
        }
        Ok(())
    }

    fn button(&self, pressed: bool) -> Result<()> {
        monitor_command(self.monitor, &format!("mouse_button {}", u8::from(pressed)))?;
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    /// Turns the wheel `notches` towards the person (which scrolls
    /// down), or away from them if negative.
    fn wheel(&self, notches: i32) -> Result<()> {
        // QEMU turns it one notch per command, up for a positive value.
        let step = if notches > 0 { -1 } else { 1 };
        for _ in 0..notches.abs() {
            monitor_command(self.monitor, &format!("mouse_move 0 0 {step}"))?;
        }
        Ok(())
    }

    fn click(&mut self, x: i32, y: i32) -> Result<()> {
        self.move_to(x, y)?;
        self.button(true)?;
        self.button(false)
    }
}

/// Labels of the programs on the desktop's task bar; must match
/// `userspace/programs/src/bin/desktop/main.rs`.
const DESKTOP_LAUNCHERS: &[&str] = &[
    "Files",
    "Run",
    "Notes",
    "Settings",
    "Calculator",
    "System Monitor",
    "About",
    "Scribble",
    "Clock",
    "Pop",
];

/// Operates the graphical desktop, which the caller has just started from
/// the serial console, and checks what it shows:
///
/// 1. the desktop appears, with its task bar and the About window;
/// 2. a click on the task bar opens the Run window, which takes the
///    keyboard focus (the About window's title bar turns grey);
/// 3. commands typed on the emulated keyboard run a native program and a
///    Windows program, and their output appears in the window; error
///    output appears too; a program that reads its input receives a typed
///    line and the end of input;
/// 4. dragging the title bar moves the window, contents included;
/// 5. the Exit button ends the desktop.
fn desktop_scenario(check: &DisplayCheck) -> Result<()> {
    use omit_gfx::desktop::{BORDER, Control, Desktop, TASKBAR_HEIGHT, TITLE_HEIGHT, wallpaper_at};
    use omit_gfx::font::{MEDIUM_13, SEMIBOLD_20};
    let patience = Duration::from_secs(30);
    let theme = omit_gfx::theme::Theme::default();
    // What shows where nothing is drawn: the wallpaper, and on the task
    // bar the bar's glass over it.
    let wallpaper = |image: &Screenshot, x: usize, y: usize| {
        rgb(wallpaper_at(
            &theme,
            image.width as i32,
            image.height as i32,
            x as i32,
            y as i32,
        ))
    };
    let bar_glass = |image: &Screenshot, x: usize, y: usize| {
        rgb(theme.bar.over(wallpaper_at(
            &theme,
            image.width as i32,
            image.height as i32,
            x as i32,
            y as i32,
        )))
    };
    // A window's title as it is written in the window in front, and in
    // the others.
    let title = |image: &Screenshot, text: &str| {
        screen_writes(image, text, MEDIUM_13, theme.text, theme.chrome)
    };

    let image = wait_for_screen(check, "the desktop", patience, |image| {
        // The first frame is copied to the screen row by row, so wait for
        // its last part, the task bar at the bottom, as well.
        image.pixel(image.width - 3, 3) == wallpaper(image, image.width - 3, 3)
            && image.pixel(image.width / 2, image.height - 2)
                == bar_glass(image, image.width / 2, image.height - 2)
            && screen_writes(image, "Omit OS", SEMIBOLD_20, theme.text, theme.window)
    })?;
    if !title(&image, "About Omit OS") {
        return Err("the About window's title is not that of the window in front".into());
    }
    // The Start mark, in the accent colour, at the left of the task bar.
    let bar_rows = image.height - TASKBAR_HEIGHT as usize..image.height;
    if image.count(bar_rows, rgb(theme.accent_text)) < 20 {
        return Err("the task bar shows no Start mark".into());
    }

    // The same layout code the desktop uses says where its buttons are.
    let mut layout = Desktop::new(image.width, image.height);
    for label in DESKTOP_LAUNCHERS {
        layout.add_launcher(label, "doc", |_| {});
    }
    let button = |label: &str| {
        let index = DESKTOP_LAUNCHERS.iter().position(|l| *l == label)?;
        let frame = layout.launcher_frame(index)?;
        Some((frame.x + frame.w / 2, frame.y + frame.h / 2))
    };
    let (run_x, run_y) = button("Run").ok_or("no Run button")?;
    let (settings_x, settings_y) = button("Settings").ok_or("no Settings button")?;
    let (calculator_x, calculator_y) = button("Calculator").ok_or("no Calculator button")?;
    let (monitor_x, monitor_y) = button("System Monitor").ok_or("no System Monitor button")?;
    let (notes_x, notes_y) = button("Notes").ok_or("no Notes button")?;
    let (files_x, files_y) = button("Files").ok_or("no Files button")?;
    debug_assert!(run_y >= image.height as i32 - TASKBAR_HEIGHT);
    let start = layout.start_button();
    let (start_x, start_y) = (start.x + start.w / 2, start.y + start.h / 2);
    // Where the power button is once the Start panel is open: ask a
    // second copy of the layout, with its panel opened.
    let ((power_x, power_y), files_row) = {
        let mut opened = Desktop::new(image.width, image.height);
        for label in DESKTOP_LAUNCHERS {
            opened.add_launcher(label, "doc", |_| {});
        }
        let at = opened.pointer();
        opened.mouse(start_x - at.x, start_y - at.y, 0);
        opened.mouse(0, 0, 1);
        opened.mouse(0, 0, 0);
        let power = opened
            .power_button_frame()
            .ok_or("the Start panel did not open in the layout")?;
        let row = opened
            .start_item_frame(0)
            .ok_or("the Start panel lists no programs in the layout")?;
        (
            (power.x + power.w / 2, power.y + power.h / 2),
            (row.x + row.w / 2, row.y + row.h / 2),
        )
    };

    let mut mouse = Mouse::new(&check.monitor)?;
    mouse.click(run_x, run_y)?;
    wait_for_screen(check, "the Run window", patience, |image| {
        screen_shows(image, "Type a program name and press Enter, for example:")
            && title(image, "Run")
            // The About window is behind now: its title is no longer
            // written as that of the window in front. (How it is
            // written instead cannot be matched exactly: the shadow of
            // the Run window falls across it.)
            && !title(image, "About Omit OS")
    })?;

    // `hello`, then `winhello.exe`.
    type_on_keyboard(&check.monitor, &["h", "e", "l", "l", "o", "ret"])?;
    wait_for_screen(check, "the output of `hello`", patience, |image| {
        screen_shows(image, "Hello from user mode! I am process")
    })?;
    type_on_keyboard(
        &check.monitor,
        &[
            "w", "i", "n", "h", "e", "l", "l", "o", "dot", "e", "x", "e", "ret",
        ],
    )?;
    wait_for_screen(check, "the output of `winhello.exe`", patience, |image| {
        screen_shows(image, "Hello from a Windows executable!")
            // The prompt is back: the program has been reaped.
            && screen_shows(image, "> _")
    })?;

    // What a program writes to standard error is shown too.
    type_on_keyboard(
        &check.monitor,
        &[
            "l", "s", "spc", "slash", "n", "o", "s", "u", "c", "h", "ret",
        ],
    )?;
    wait_for_screen(check, "an error message from `ls`", patience, |image| {
        screen_shows(image, "ls: /nosuch: No such file or directory") && screen_shows(image, "> _")
    })?;
    // A program that reads its input: `tee` gets the typed line through
    // a pipe, shows it and writes it to a file (checked by the caller
    // after the desktop has closed); Ctrl+D ends its input.
    type_on_keyboard(
        &check.monitor,
        &[
            "t", "e", "e", "spc", "slash", "t", "y", "p", "e", "d", "ret",
        ],
    )?;
    wait_for_screen(check, "tee waiting for input", patience, |image| {
        screen_shows(image, "tee< _")
    })?;
    type_on_keyboard(
        &check.monitor,
        &[
            "k", "e", "y", "b", "o", "a", "r", "d", "spc", "i", "n", "p", "u", "t", "ret",
        ],
    )?;
    wait_for_screen(check, "the line echoed by tee", patience, |image| {
        screen_shows(image, "keyboard input") && screen_shows(image, "tee< _")
    })?;
    type_on_keyboard(&check.monitor, &["ctrl-d"])?;
    let before = wait_for_screen(check, "the prompt after tee ended", patience, |image| {
        screen_shows(image, "> _")
    })?;

    // A program with a window of its own. `scribble` is the third window
    // opened; the desktop's own layout code says where it appears.
    struct Blank;
    impl omit_gfx::desktop::App for Blank {
        fn title(&self) -> &str {
            ""
        }
        fn draw(
            &mut self,
            _: &mut omit_gfx::surface::Canvas<'_>,
            _: i32,
            _: i32,
            _: &omit_gfx::theme::Theme,
        ) {
        }
    }
    layout.open(Box::new(Blank), 470, 230);
    let run_window = layout.open(Box::new(Blank), 620, 360);
    let scribble = layout.open(Box::new(Blank), 360, 240);
    let frame = layout
        .window_frame(scribble)
        .ok_or("no frame for the third window")?;
    // The content area's corner, and colours from `scribble.rs`.
    let (left, top) = (frame.x + BORDER, frame.y + BORDER + TITLE_HEIGHT);
    let at =
        |image: &Screenshot, x: i32, y: i32| image.pixel((left + x) as usize, (top + y) as usize);
    let (paper, black, red) = (rgb(0xFF_FF_F0), rgb(0x20_20_20), rgb(0xC0_20_20));
    type_on_keyboard(
        &check.monitor,
        &["s", "c", "r", "i", "b", "b", "l", "e", "ret"],
    )?;
    wait_for_screen(check, "the Scribble window", patience, |image| {
        title(image, "Scribble") && at(image, 10, 100) == paper && at(image, 10, 3) == black
    })?;
    // Draw a stroke: press, move, release. The program hears about all
    // three and draws a line between the two points.
    mouse.move_to(left + 50, top + 50)?;
    mouse.button(true)?;
    mouse.move_to(left + 150, top + 120)?;
    mouse.button(false)?;
    // (Away, so that the pointer does not cover what is checked.)
    mouse.move_to(left + 300, top + 200)?;
    wait_for_screen(
        check,
        "a stroke in the Scribble window",
        patience,
        |image| {
            at(image, 50, 50) == black
                && at(image, 100, 85) == black
                && at(image, 150, 120) == black
                && at(image, 100, 150) == paper
                && title(image, "Scribble - 1 stroke(s)")
        },
    )?;
    // Keys go to the program: `2` selects red, `c` clears the drawing.
    type_on_keyboard(&check.monitor, &["2"])?;
    wait_for_screen(check, "the red colour strip", patience, |image| {
        at(image, 10, 3) == red && at(image, 100, 85) == black
    })?;
    type_on_keyboard(&check.monitor, &["c"])?;
    wait_for_screen(check, "a cleared Scribble window", patience, |image| {
        at(image, 100, 85) == paper && at(image, 10, 3) == red && title(image, "Scribble")
    })?;
    // `q` ends the program; its window goes away and Run, which started
    // it, has its prompt back.
    type_on_keyboard(&check.monitor, &["q"])?;
    wait_for_screen(
        check,
        "the desktop without the Scribble window",
        patience,
        |image| {
            screen_shows(image, "> _") && at(image, 10, 100) != paper && at(image, 10, 3) != red
        },
    )?;

    // A graphical Windows program: its window is the fourth one opened.
    let wingui = layout.open(Box::new(Blank), 340, 150);
    let frame = layout
        .window_frame(wingui)
        .ok_or("no frame for the fourth window")?;
    let (left, top) = (frame.x + BORDER, frame.y + BORDER + TITLE_HEIGHT);
    // Text as the Windows program draws it: black on white.
    let win_text = |image: &Screenshot, text: &str| screen_shows_in(image, text, 0, 0xFF_FF_FF);
    type_on_keyboard(
        &check.monitor,
        &["w", "i", "n", "g", "u", "i", "dot", "e", "x", "e", "ret"],
    )?;
    wait_for_screen(check, "the window of wingui.exe", patience, |image| {
        title(image, "Windows GUI test")
            && win_text(image, "Hello from a Windows GUI program")
            && win_text(image, "clicks: 0")
            // Drawn through GetDC, outside WM_PAINT: a green square
            // with a yellow pixel in the middle.
            && image.pixel((left + 305) as usize, (top + 15) as usize) == rgb(0x14_A0_3C)
            && image.pixel((left + 310) as usize, (top + 20) as usize) == rgb(0xFF_FF_00)
    })?;
    // A click: WM_LBUTTONDOWN with the position, then a repaint with a
    // red square there.
    mouse.click(left + 200, top + 100)?;
    mouse.move_to(left + 300, top + 130)?;
    wait_for_screen(check, "the click shown by wingui.exe", patience, |image| {
        win_text(image, "clicks: 1")
            // (The repaint erased what was drawn outside WM_PAINT.)
            && image.pixel((left + 305) as usize, (top + 15) as usize) == rgb(0xFF_FF_FF)
            && image.pixel((left + 200) as usize, (top + 100) as usize) == rgb(0xC8_1E_1E)
            && image.pixel((left + 200) as usize, (top + 80) as usize) == rgb(0xFF_FF_FF)
    })?;
    // Keys arrive as WM_CHAR; `t` makes the program set its title.
    type_on_keyboard(&check.monitor, &["a", "b", "t"])?;
    wait_for_screen(check, "typed text and a new title", patience, |image| {
        win_text(image, "typed: ab") && title(image, "1 click(s)")
    })?;
    // Kept next to the serial log: a picture of a Windows program's
    // window on the desktop.
    let _ = fs::copy(
        &check.screenshot,
        check.screenshot.with_extension("windows-gui.ppm"),
    );
    // `x`: the program starts a 50 ms timer that it stops after five
    // WM_TIMER messages.
    type_on_keyboard(&check.monitor, &["x"])?;
    wait_for_screen(check, "five timer ticks", patience, |image| {
        win_text(image, "ticks: 5") && win_text(image, "typed: ab")
    })?;
    // `m`: a message box, a window of its own, closed with Enter.
    type_on_keyboard(&check.monitor, &["m"])?;
    let box_text =
        |image: &Screenshot| screen_shows_in(image, "This is a message box.", 0, 0xF0_F0_F0);
    wait_for_screen(check, "the message box", patience, |image| {
        box_text(image) && title(image, "wingui")
    })?;
    type_on_keyboard(&check.monitor, &["ret"])?;
    wait_for_screen(
        check,
        "the window without the message box",
        patience,
        |image| !box_text(image) && title(image, "1 click(s)") && win_text(image, "typed: ab"),
    )?;
    // `q`: the program destroys its window, leaves its message loop and
    // reports what it saw.
    type_on_keyboard(&check.monitor, &["q"])?;
    wait_for_screen(check, "the report of wingui.exe", patience, |image| {
        screen_shows(
            image,
            "wingui: code 7, 1 click(s), typed \"ab\", checks 0xf, paints ok, ticks 5",
        ) && screen_shows(image, "wingui: 39 startup checks passed")
            && screen_shows(image, "> _")
    })?;

    // A game, redrawing fifty times a second. In test mode its discs
    // appear at known places and stay. (The message box was the fifth
    // window; the game's is the sixth.)
    layout.open(Box::new(Blank), 200, 100);
    let pop = layout.open(Box::new(Blank), 400, 300);
    let frame = layout
        .window_frame(pop)
        .ok_or("no frame for the sixth window")?;
    let (left, top) = (frame.x + BORDER, frame.y + BORDER + TITLE_HEIGHT);
    let at =
        |image: &Screenshot, x: i32, y: i32| image.pixel((left + x) as usize, (top + y) as usize);
    let status =
        |image: &Screenshot, text: &str| screen_shows_in(image, text, 0xF0_F4_FF, 0x1E_2A_44);
    type_on_keyboard(
        &check.monitor,
        &[
            "p", "o", "p", "spc", "minus", "minus", "t", "e", "s", "t", "ret",
        ],
    )?;
    wait_for_screen(check, "the game's first disc", patience, |image| {
        title(image, "Pop")
            && status(image, "score: 0   lives: 3")
            && at(image, 100, 120) == rgb(0xE8_5D_4A)
    })?;
    // A click on the disc pops it; the next one appears elsewhere.
    mouse.click(left + 100, top + 120)?;
    mouse.move_to(left + 20, top + 280)?;
    wait_for_screen(check, "the score after popping a disc", patience, |image| {
        status(image, "score: 1   lives: 3")
            && at(image, 100, 120) == rgb(0x12_1A_2B)
            && at(image, 280, 200) == rgb(0x4A_B8_E8)
    })?;
    // A click beside the disc changes nothing. Then let the game run for
    // a few seconds, so that the frame pacing figures it reports on exit
    // cover more than a moment.
    mouse.click(left + 200, top + 250)?;
    mouse.move_to(left + 20, top + 280)?;
    thread::sleep(Duration::from_secs(4));
    type_on_keyboard(&check.monitor, &["q"])?;
    wait_for_screen(check, "the game's report", patience, |image| {
        screen_shows(image, "pop: score 1;") && screen_shows(image, "> _")
    })?;
    // Kept for the frame pacing figures the game reports.
    let _ = fs::copy(
        &check.screenshot,
        check.screenshot.with_extension("pop-report.ppm"),
    );

    // The Run window is the second one opened; drag it by its title bar
    // (the About window does not reach this far right).
    let (grab_x, grab_y) = (560, 70);
    if before.pixel(grab_x as usize, grab_y as usize) != rgb(theme.chrome) {
        return Err("the Run window's title bar is not where it is expected".into());
    }
    let probe = (640usize, 300usize);
    if before.pixel(probe.0, probe.1) != rgb(theme.window) {
        return Err("the Run window does not cover the probe point".into());
    }
    mouse.move_to(grab_x, grab_y)?;
    mouse.button(true)?;
    mouse.move_to(grab_x + 420, grab_y + 330)?;
    mouse.button(false)?;
    wait_for_screen(
        check,
        "the Run window in its new place",
        patience,
        |image| {
            image.pixel(probe.0, probe.1) == wallpaper(image, probe.0, probe.1)
                // (A point of the title bar that the pointer, which is
                // now at the grab point, does not cover.)
                && image.pixel(grab_x as usize + 320, grab_y as usize + 330)
                    == rgb(theme.chrome)
                && screen_shows(image, "Hello from a Windows executable!")
        },
    )?;

    // Maximise the Run window with the button in its title bar — it then
    // covers the screen above the task bar — and put it back.
    let moved = layout
        .window_frame(run_window)
        .ok_or("no frame for the Run window")?
        .translate(420, 330);
    // The maximise button keeps its distance from the window's top-right
    // corner wherever and however large the window is.
    let (from_right, from_top) = {
        let frame = layout
            .window_frame(run_window)
            .ok_or("no frame for the Run window")?;
        let button = layout
            .control_frame(run_window, Control::Maximise)
            .ok_or("the Run window has no maximise button")?;
        (
            frame.right() - (button.x + button.w / 2),
            button.y + button.h / 2 - frame.y,
        )
    };
    let maximise =
        |frame: omit_gfx::geometry::Rect| (frame.right() - from_right, frame.y + from_top);
    let (x, y) = maximise(moved);
    mouse.click(x, y)?;
    let far = (image.width - 10, 400usize);
    wait_for_screen(check, "the Run window maximised", patience, |image| {
        image.pixel(far.0, far.1) == rgb(theme.window)
            && image.pixel(20, 400) == rgb(theme.window)
            // (Square corners when maximised: the title bar reaches the
            // screen's corner.)
            && image.pixel(image.width / 2, 4) == rgb(theme.chrome)
            && image.pixel(3, 3) == rgb(theme.chrome)
            && image.pixel(image.width / 2, image.height - 2)
                == bar_glass(image, image.width / 2, image.height - 2)
            && screen_shows(image, "Hello from a Windows executable!")
    })?;
    let full = omit_gfx::geometry::Rect::new(
        0,
        0,
        image.width as i32,
        image.height as i32 - TASKBAR_HEIGHT,
    );
    let (x, y) = maximise(full);
    mouse.click(x, y)?;
    mouse.move_to(20, 300)?;
    wait_for_screen(check, "the Run window restored", patience, |image| {
        image.pixel(far.0, far.1) == wallpaper(image, far.0, far.1)
            && image.pixel(20, 400) == wallpaper(image, 20, 400)
            && image.pixel(grab_x as usize + 320, grab_y as usize + 330) == rgb(theme.chrome)
    })?;

    // Minimise the Run window with its button; its task bar icon brings
    // it back.
    let minimise = {
        let frame = layout
            .window_frame(run_window)
            .ok_or("no frame for the Run window")?;
        let button = layout
            .control_frame(run_window, Control::Minimise)
            .ok_or("the Run window has no minimise button")?;
        (
            moved.right() - (frame.right() - (button.x + button.w / 2)),
            moved.y + (button.y + button.h / 2 - frame.y),
        )
    };
    mouse.click(minimise.0, minimise.1)?;
    mouse.move_to(20, 300)?;
    wait_for_screen(
        check,
        "the desktop without the Run window",
        patience,
        |image| {
            image.pixel(grab_x as usize + 320, grab_y as usize + 330)
            == wallpaper(image, grab_x as usize + 320, grab_y as usize + 330)
            && !screen_shows(image, "Hello from a Windows executable!")
            // The About window is in front again.
            && title(image, "About Omit OS")
        },
    )?;
    mouse.click(run_x, run_y)?;
    mouse.move_to(20, 300)?;
    wait_for_screen(
        check,
        "the Run window back from the task bar",
        patience,
        |image| {
            image.pixel(grab_x as usize + 320, grab_y as usize + 330) == rgb(theme.chrome)
                && screen_shows(image, "Hello from a Windows executable!")
                && title(image, "Run")
        },
    )?;

    // Settings: the seventh window. Choosing "Dark" repaints everything
    // at once — the wallpaper becomes the dusk picture and the window's
    // own title is written light on dark — and "Light" puts it back.
    {
        use omit_gfx::settings::{self, Choice};
        use omit_gfx::theme::{Accent, Mode, Theme};
        let window = layout.open(Box::new(Blank), settings::SIZE.0, settings::SIZE.1);
        let frame = layout
            .window_frame(window)
            .ok_or("no frame for the Settings window")?;
        let (left, top) = (frame.x + BORDER, frame.y + BORDER + TITLE_HEIGHT);
        let middle_of = |choice| {
            let area = settings::control(choice);
            (left + area.x + area.w / 2, top + area.y + area.h / 2)
        };
        mouse.click(settings_x, settings_y)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the Settings window", patience, |image| {
            title(image, "Settings")
                && screen_writes(image, "Look & feel", SEMIBOLD_20, theme.text, theme.window)
        })?;
        let dark = Theme::new(Mode::Dark, Accent::Blue);
        let far = (image.width - 10, 100usize);
        let (x, y) = middle_of(Choice::Mode(Mode::Dark));
        mouse.click(x, y)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the desktop in dark mode", patience, |image| {
            image.pixel(far.0, far.1)
                == rgb(wallpaper_at(
                    &dark,
                    image.width as i32,
                    image.height as i32,
                    far.0 as i32,
                    far.1 as i32,
                ))
                && screen_writes(image, "Settings", MEDIUM_13, dark.text, dark.chrome)
                && screen_writes(image, "Look & feel", SEMIBOLD_20, dark.text, dark.window)
        })?;
        // Kept next to the serial log: the desktop in dark mode.
        let _ = fs::copy(
            &check.screenshot,
            check.screenshot.with_extension("dark.ppm"),
        );
        let (x, y) = middle_of(Choice::Mode(Mode::Light));
        mouse.click(x, y)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(
            check,
            "the desktop in light mode again",
            patience,
            |image| {
                image.pixel(far.0, far.1) == wallpaper(image, far.0, far.1)
                    && title(image, "Settings")
            },
        )?;
        // Another accent: the Start mark on the task bar takes its
        // colour. (It stays chosen for the rest of the test, and is
        // what the settings file must say afterwards.)
        let teal = Theme::new(Mode::Light, Accent::Teal);
        let (x, y) = middle_of(Choice::Accent(Accent::Teal));
        mouse.click(x, y)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the teal accent", patience, |image| {
            let bar = image.height - TASKBAR_HEIGHT as usize..image.height;
            image.count(bar.clone(), rgb(teal.accent)) > 20
                && image.count(bar, rgb(theme.accent)) == 0
        })?;
    }
    // (From here on the accent is teal.)
    let theme =
        omit_gfx::theme::Theme::new(omit_gfx::theme::Mode::Light, omit_gfx::theme::Accent::Teal);

    // Files: the eighth window. It lists the top folder; a click on the
    // first row (`bin`: folders come first) selects it and a second
    // click opens it.
    {
        use omit_gfx::files::{self, Entry, Files, Storage};
        use omit_gfx::font::REGULAR_13;
        // Only the geometry is asked of this copy: any folder with a
        // first row will do.
        struct OneFolder;
        impl Storage for OneFolder {
            fn list(&mut self, _: &str) -> std::result::Result<Vec<Entry>, String> {
                Ok(vec![Entry {
                    name: "bin".into(),
                    is_dir: true,
                    size: 0,
                }])
            }
        }
        let geometry = Files::new(OneFolder, Vec::new(), "/", Box::new(|_, _| {}));
        let row = geometry.row_frame(0).ok_or("no first row in Files")?;
        let window = layout.open(Box::new(Blank), files::SIZE.0, files::SIZE.1);
        let frame = layout
            .window_frame(window)
            .ok_or("no frame for the Files window")?;
        let (left, top) = (frame.x + BORDER, frame.y + BORDER + TITLE_HEIGHT);
        mouse.click(files_x, files_y)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the Files window", patience, |image| {
            title(image, "Files — Computer")
                && screen_writes(image, "Programs", REGULAR_13, theme.text, theme.chrome)
                && screen_writes(image, "bin", REGULAR_13, theme.text, theme.window)
        })?;
        let (x, y) = (left + row.x + row.w / 2, top + row.y + row.h / 2);
        mouse.click(x, y)?;
        mouse.click(x, y)?;
        mouse.move_to(20, 300)?;
        let saved = wait_for_screen(check, "the folder bin in Files", patience, |image| {
            title(image, "Files — bin")
                && screen_writes(image, "fetch", REGULAR_13, theme.text, theme.window)
        })?;
        let _ = saved;
        // Kept next to the serial log: the desktop with Files in front.
        let _ = fs::copy(
            &check.screenshot,
            check.screenshot.with_extension("files.ppm"),
        );

        // Arranging: the Files window dragged by its title bar to the
        // right edge of the screen takes the right half when let go.
        let half = layout.zone_frame(omit_gfx::desktop::Zone::Right);
        let inside = ((half.x + half.w / 2) as usize, (half.y + 5) as usize);
        let beside = ((half.x - 5) as usize, (half.y + 5) as usize);
        mouse.move_to(frame.x + 150, frame.y + 20)?;
        mouse.button(true)?;
        mouse.move_to(image.width as i32 - 1, 300)?;
        mouse.button(false)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(
            check,
            "Files in the right half of the screen",
            patience,
            |image| {
                image.pixel(inside.0, inside.1) == rgb(theme.chrome)
                    && image.pixel(beside.0, beside.1) != rgb(theme.chrome)
                    && title(image, "Files — bin")
            },
        )?;

        // A new folder, made with the tool bar's button and named by
        // typing over the name it starts with. (That it exists is
        // looked at after the desktop has closed.)
        use omit_gfx::desktop::App;
        use omit_gfx::files::Action;
        use omit_gfx::font::REGULAR_12;
        let (width, height) = (half.w - 2 * BORDER, half.h - 2 * BORDER - TITLE_HEIGHT);
        let mut arranged = Files::new(OneFolder, Vec::new(), "/", Box::new(|_, _| {}));
        let mut scratch = omit_gfx::surface::Surface::new(width as usize, height as usize, 0);
        arranged.draw(&mut scratch.canvas(), width, height, &theme);
        let button = arranged.action_button(Action::NewFolder);
        mouse.click(
            half.x + BORDER + button.x + button.w / 2,
            half.y + BORDER + TITLE_HEIGHT + button.y + button.h / 2,
        )?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the new folder in Files", patience, |image| {
            screen_writes(
                image,
                "New folder selected",
                REGULAR_12,
                theme.text_3,
                theme.window,
            )
        })?;
        type_on_keyboard(&check.monitor, &["s", "t", "u", "f", "f", "ret"])?;
        wait_for_screen(check, "the folder named in Files", patience, |image| {
            screen_writes(
                image,
                "stuff selected",
                REGULAR_12,
                theme.text_3,
                theme.window,
            )
        })?;

        // The wheel: with the pointer over the list, turning it towards
        // the person brings the end of the list into view, and turning
        // it back the beginning.
        let last_name = |image: &Screenshot| {
            screen_writes(image, "winthread.exe", REGULAR_13, theme.text, theme.window)
        };
        let first_name = |image: &Screenshot| {
            screen_writes(image, "clock", REGULAR_13, theme.text, theme.window)
        };
        mouse.move_to(half.x + half.w / 2, half.y + half.h / 2)?;
        let image = wait_for_screen(check, "the start of the list in Files", patience, |image| {
            first_name(image)
        })?;
        if last_name(&image) {
            return Err("the list in Files is too short to scroll".into());
        }
        mouse.wheel(10)?;
        wait_for_screen(
            check,
            "the end of the list after the wheel",
            patience,
            |image| last_name(image) && !first_name(image),
        )?;
        mouse.wheel(-10)?;
        wait_for_screen(check, "the start of the list again", patience, |image| {
            first_name(image) && !last_name(image)
        })?;
        mouse.move_to(20, 300)?;
    }

    // The Calculator: the ninth window. 7 × 6 = by mouse; the result is
    // written large. Then − 2 and Enter on the keyboard.
    {
        use omit_gfx::calculator::{self, Key, Operator};
        use omit_gfx::font::SEMIBOLD_32;
        let window = layout.open(Box::new(Blank), calculator::SIZE.0, calculator::SIZE.1);
        let frame = layout
            .window_frame(window)
            .ok_or("no frame for the Calculator window")?;
        let (left, top) = (frame.x + BORDER, frame.y + BORDER + TITLE_HEIGHT);
        mouse.click(calculator_x, calculator_y)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the Calculator window", patience, |image| {
            title(image, "Calculator")
        })?;
        for key in [
            Key::Digit(7),
            Key::Operator(Operator::Multiply),
            Key::Digit(6),
            Key::Equals,
        ] {
            let area = calculator::key_frame(key).ok_or("a key without a place")?;
            mouse.click(left + area.x + area.w / 2, top + area.y + area.h / 2)?;
        }
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the result of 7 x 6", patience, |image| {
            screen_writes(image, "42", SEMIBOLD_32, theme.text, theme.window)
        })?;
        type_on_keyboard(&check.monitor, &["minus", "2", "ret"])?;
        wait_for_screen(check, "the result of 42 - 2", patience, |image| {
            screen_writes(image, "40", SEMIBOLD_32, theme.text, theme.window)
        })?;
    }

    // System Monitor: the tenth window. Its list has the kernel's line
    // and the desktop itself; the other page says how much memory is in
    // use.
    {
        use omit_gfx::font::{MEDIUM_13, REGULAR_13};
        use omit_gfx::monitor::{self, Monitor};
        // Only the geometry is asked of this copy.
        struct Nothing;
        impl monitor::Source for Nothing {
            fn now_ms(&mut self) -> u64 {
                0
            }
            fn sample(&mut self) -> monitor::Sample {
                monitor::Sample::default()
            }
            fn end(&mut self, _: u64) -> bool {
                false
            }
        }
        let window = layout.open(Box::new(Blank), monitor::SIZE.0, monitor::SIZE.1);
        let frame = layout
            .window_frame(window)
            .ok_or("no frame for the System Monitor window")?;
        let (left, top) = (frame.x + BORDER, frame.y + BORDER + TITLE_HEIGHT);
        mouse.click(monitor_x, monitor_y)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the System Monitor's list", patience, |image| {
            title(image, "System Monitor")
                && screen_writes(
                    image,
                    monitor::KERNEL_NAME,
                    REGULAR_13,
                    theme.text,
                    theme.window,
                )
                && screen_writes(image, "desktop", REGULAR_13, theme.text, theme.window)
        })?;
        let page = Monitor::<Nothing>::page_frame(1).ok_or("no second page")?;
        mouse.click(left + page.x + page.w / 2, top + page.y + page.h / 2)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the System Monitor's graphs", patience, |image| {
            screen_writes(
                image,
                "Memory",
                MEDIUM_13,
                theme.text_2,
                theme.window_raised,
            ) && screen_writes(
                image,
                "Processor",
                MEDIUM_13,
                theme.text_2,
                theme.window_raised,
            )
        })?;
        // Kept next to the serial log: the desktop with the monitor.
        let _ = fs::copy(
            &check.screenshot,
            check.screenshot.with_extension("monitor.ppm"),
        );
    }

    // The window switcher. A quick Alt+Tab goes to the window used
    // before this one, the Calculator. Then, with Alt+Tab held, the
    // switcher is on the screen: one of its tiles is outlined in the
    // accent colour (which one is not asked: a held Tab repeats on some
    // keyboards). When the keys are let go it is gone.
    {
        use omit_gfx::desktop::modifier;
        type_on_keyboard(&check.monitor, &["alt-tab"])?;
        wait_for_screen(check, "the Calculator after Alt+Tab", patience, |image| {
            title(image, "Calculator")
        })?;
        layout.set_modifiers(modifier::ALT);
        layout.key(9);
        let tiles: Vec<_> = (0..layout.window_count())
            .filter_map(|index| layout.switcher_tile_frame(index))
            .collect();
        layout.key(27);
        layout.set_modifiers(0);
        if tiles.len() != layout.window_count() || tiles.is_empty() {
            return Err("the switcher has no tiles to look for".into());
        }
        let outlined = |image: &Screenshot| {
            tiles.iter().any(|tile| {
                [
                    (tile.x + tile.w / 2, tile.y),
                    (tile.x + tile.w / 2, tile.y + tile.h - 1),
                    (tile.x, tile.y + tile.h / 2),
                    (tile.x + tile.w - 1, tile.y + tile.h / 2),
                ]
                .iter()
                .all(|&(x, y)| image.pixel(x as usize, y as usize) == rgb(theme.accent))
            })
        };
        monitor_command(&check.monitor, "sendkey alt-tab 6000")?;
        wait_for_screen(check, "the window switcher", patience, |image| {
            outlined(image)
        })?;
        // Kept next to the serial log: the switcher.
        let _ = fs::copy(
            &check.screenshot,
            check.screenshot.with_extension("switcher.ppm"),
        );
        wait_for_screen(check, "the switcher put away", patience, |image| {
            !outlined(image)
        })?;
    }

    // Notes: a new note, two lines typed, saved with Ctrl+S. (What is
    // in the file is looked at after the desktop has closed.)
    {
        use omit_gfx::font::REGULAR_12;
        layout.open(
            Box::new(Blank),
            omit_gfx::notes::SIZE.0,
            omit_gfx::notes::SIZE.1,
        );
        mouse.click(notes_x, notes_y)?;
        mouse.move_to(20, 300)?;
        wait_for_screen(check, "the Notes window", patience, |image| {
            title(image, "Notes — note-1.txt")
        })?;
        type_on_keyboard(
            &check.monitor,
            &[
                "h", "e", "l", "l", "o", "spc", "n", "o", "t", "e", "s", "ret", "s", "e", "c", "o",
                "n", "d",
            ],
        )?;
        wait_for_screen(check, "the note typed", patience, |image| {
            title(image, "Notes — note-1.txt •") && screen_shows(image, "hello notes")
        })?;
        type_on_keyboard(&check.monitor, &["ctrl-s"])?;
        wait_for_screen(check, "the note saved", patience, |image| {
            title(image, "Notes — note-1.txt")
                && screen_writes(image, "Saved", REGULAR_12, theme.success, theme.chrome)
        })?;
        // Kept next to the serial log: the desktop with Notes in front.
        let _ = fs::copy(
            &check.screenshot,
            check.screenshot.with_extension("notes.ppm"),
        );
    }

    // The Start mark opens the list of programs; the row under the
    // pointer is filled with the accent colour. Its power button ends
    // the desktop.
    mouse.click(start_x, start_y)?;
    mouse.move_to(files_row.0, files_row.1)?;
    wait_for_screen(check, "the Start panel", patience, |image| {
        let row = (files_row.1 - 10) as usize..(files_row.1 + 10) as usize;
        image.count(row, rgb(theme.accent)) > 2000
    })?;
    mouse.move_to(power_x, power_y)?;
    wait_for_screen(
        check,
        "the power button under the pointer",
        patience,
        |image| {
            let rows = (power_y - 12) as usize..(power_y + 12) as usize;
            image.count(rows, rgb(theme.danger)) > 300
        },
    )?;
    mouse.button(true)?;
    mouse.button(false)
}

/// Checks that a screenshot shows the kernel's console: a status bar with
/// text on the first row and light text on the dark background below.
fn check_screenshot(image: &Screenshot) -> Vec<String> {
    let mut failures = Vec::new();
    if image.width < 640 || image.height < 400 {
        failures.push(format!(
            "screenshot is only {}x{}",
            image.width, image.height
        ));
        return failures;
    }
    let bar = 0..SCREEN_ROW_HEIGHT;
    let text = SCREEN_ROW_HEIGHT..image.height;
    let text_pixels = (image.height - SCREEN_ROW_HEIGHT) * image.width;

    if image.pixel(2, 2) != SCREEN_STATUS_BACKGROUND {
        failures.push(format!(
            "top-left pixel is {:02x?}, not the status bar colour",
            image.pixel(2, 2)
        ));
    }
    // Glyphs are anti-aliased, so only their cores are pure white.
    if image.count(bar.clone(), SCREEN_STATUS_FOREGROUND) < 30 {
        failures.push("status bar shows no text".into());
    }
    if image.count(bar, SCREEN_BACKGROUND) != 0 {
        failures.push("status bar does not span the screen width".into());
    }
    if image.count(text.clone(), SCREEN_BACKGROUND) < text_pixels / 2 {
        failures.push("text area does not have the console background".into());
    }
    // (A screen of short lines has few such pixels: a listing of forty
    // names and a dozen other lines measured 397, 128 of them the
    // cursor. An empty screen with only the cursor must not pass.)
    if image.count(text.clone(), SCREEN_FOREGROUND) < 200 {
        failures.push("text area shows no text".into());
    }
    if image.count(text, SCREEN_STATUS_BACKGROUND) != 0 {
        failures.push("status bar colour leaked into the text area".into());
    }
    failures
}

/// Checks the ext2 filesystem in `image` with `e2fsck`, an implementation
/// independent of the kernel's driver. Returns the problems found. If
/// `e2fsck` is not installed the check is skipped with a note.
fn check_ext2(image: &Path) -> Vec<String> {
    let Some(e2fsck) = ["/usr/sbin/e2fsck", "/sbin/e2fsck", "/usr/bin/e2fsck"]
        .iter()
        .find(|path| Path::new(path).exists())
    else {
        eprintln!("xtask: note: e2fsck not found; skipping the check of the data disk");
        return Vec::new();
    };
    match Command::new(e2fsck).arg("-fn").arg(image).output() {
        Ok(output) if output.status.success() => Vec::new(),
        Ok(output) => vec![format!(
            "e2fsck found problems on the data disk:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )],
        Err(error) => vec![format!("cannot run e2fsck: {error}")],
    }
}

/// Checks a disk the system was installed on, independently of the
/// installer's own code paths in the kernel: the partition table is read
/// back, the files on the EFI System Partition are read with the `fatfs`
/// crate and compared with the build products, and the data partition is
/// checked with `e2fsck`.
fn check_installed_disk(
    disk: &Path,
    artifacts: &Artifacts,
    cmdline: &str,
    dir: &Path,
) -> Vec<String> {
    use omit_gpt::{SECTOR_SIZE, TYPE_EFI_SYSTEM, TYPE_OMIT_DATA};
    let data = match fs::read(disk) {
        Ok(data) => data,
        Err(error) => return vec![format!("cannot read the installed disk: {error}")],
    };
    let partitions = omit_gpt::parse(data.len() as u64 / SECTOR_SIZE, |lba, buffer| {
        let start = (lba * SECTOR_SIZE) as usize;
        data.get(start..start + buffer.len())
            .map(|sector| buffer.copy_from_slice(sector))
            .is_some()
    });
    let partitions = match partitions {
        Ok(partitions) => partitions,
        Err(error) => return vec![format!("installed disk: {error}")],
    };
    let types: Vec<_> = partitions.iter().map(|p| p.type_guid).collect();
    if types != [TYPE_EFI_SYSTEM, TYPE_OMIT_DATA] {
        return vec![format!(
            "installed disk: unexpected partitions {partitions:?}"
        )];
    }
    let bytes_of = |index: usize| {
        let partition = &partitions[index];
        let start = (partition.first_lba * SECTOR_SIZE) as usize;
        data[start..start + (partition.sectors() * SECTOR_SIZE) as usize].to_vec()
    };

    let mut failures = Vec::new();
    let mut system = bytes_of(0);
    match fatfs::FileSystem::new(Cursor::new(&mut system[..]), fatfs::FsOptions::new()) {
        Ok(volume) => {
            let expected: [(&str, Vec<u8>); 4] = [
                (
                    "EFI/BOOT/BOOTX64.EFI",
                    fs::read(&artifacts.loader).unwrap_or_default(),
                ),
                ("EFI/OMIT/KERNEL.ELF", artifacts.kernel_image.clone()),
                ("EFI/OMIT/INITRD", artifacts.initrd.clone()),
                ("EFI/OMIT/CMDLINE", cmdline.as_bytes().to_vec()),
            ];
            for (path, contents) in expected {
                let mut found = Vec::new();
                let read = volume
                    .root_dir()
                    .open_file(path)
                    .and_then(|mut file| file.read_to_end(&mut found));
                match read {
                    Ok(_) if found == contents && !contents.is_empty() => {}
                    Ok(_) => failures.push(format!(
                        "installed disk: {path} differs from the build product"
                    )),
                    Err(error) => failures.push(format!("installed disk: {path}: {error}")),
                }
            }
        }
        Err(error) => failures.push(format!(
            "installed disk: the system partition is not a readable FAT filesystem: {error}"
        )),
    }

    let extracted = dir.join("installed-data-partition.img");
    match fs::write(&extracted, bytes_of(1)) {
        Ok(()) => failures.extend(check_ext2(&extracted)),
        Err(error) => failures.push(format!("cannot extract the data partition: {error}")),
    }
    let _ = fs::remove_file(&extracted);
    failures
}

/// Finds the first shell prompt (`omit:<cwd># `) at or after `from` and
/// returns the offset just past it.
fn find_prompt(output: &[u8], from: usize) -> Option<usize> {
    let find = |needle: &[u8], start: usize| {
        output
            .get(start..)?
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|index| start + index)
    };
    let start = find(b"omit:", from)?;
    find(b"# ", start).map(|index| index + 2)
}

/// Runs QEMU to completion, capturing the serial console.
///
/// If `script` is non-empty it is typed into the shell line by line, each
/// line only after the previous one's prompt has appeared, followed by
/// `exit`. Nothing is sent before the first prompt: earlier input would be
/// consumed by the firmware. Script failures are appended to `failures`.
/// Runs a machine that has no serial port: waits until the screen shows
/// the console, then types `exit` on the keyboard — again every few
/// seconds, since nothing says when the shell is ready to read — until
/// QEMU ends. The exit status is the only thing the kernel can report.
fn run_without_serial(
    mut command: Command,
    display: &DisplayCheck,
    timeout: Duration,
    failures: &mut Vec<String>,
) -> Result<QemuRun> {
    let start = Instant::now();
    let deadline = start + timeout;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start QEMU: {e}"))?;
    let mut console_seen = false;
    let mut last_typed = Instant::now();
    let exit = loop {
        if let Some(status) = child.try_wait()? {
            break status.code();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        if !console_seen {
            // (The monitor socket appears a moment after QEMU starts.)
            if let Ok(image) = take_screenshot(&display.monitor, &display.screenshot)
                && check_screenshot(&image).is_empty()
            {
                console_seen = true;
                last_typed = Instant::now() - Duration::from_secs(10);
            }
        } else if last_typed.elapsed() >= Duration::from_secs(3) {
            let _ = type_on_keyboard(&display.monitor, &["e", "x", "i", "t", "ret"]);
            last_typed = Instant::now();
        }
        thread::sleep(Duration::from_millis(100));
    };
    if !console_seen {
        failures.push("the console never appeared on the screen".into());
    }
    Ok(QemuRun {
        exit,
        serial: String::new(),
        elapsed: start.elapsed(),
    })
}

fn run_qemu_captured(
    mut command: Command,
    script: &[(&str, &str)],
    ends_with_reset: bool,
    display: Option<&DisplayCheck>,
    timeout: Duration,
    failures: &mut Vec<String>,
) -> Result<QemuRun> {
    let start = Instant::now();
    let deadline = start + timeout;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start QEMU: {e}"))?;

    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");

    let output = Arc::new(Mutex::new(Vec::new()));
    let reader = {
        let output = Arc::clone(&output);
        thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(count) = stdout.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                output
                    .lock()
                    .expect("output lock")
                    .extend_from_slice(&chunk[..count]);
            }
        })
    };
    let stderr_reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    // Waits until a prompt appears at or after `from`.
    let wait_for_prompt = |from: usize| -> Option<usize> {
        loop {
            if let Some(end) = find_prompt(&output.lock().expect("output lock"), from) {
                return Some(end);
            }
            if Instant::now() >= deadline || reader.is_finished() {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    };

    if !script.is_empty() {
        let mut position = wait_for_prompt(0);
        if position.is_none() {
            failures.push("shell prompt never appeared".into());
        }
        for (line, expected) in script {
            let Some(from) = position else { break };
            // A line ending in Ctrl-D ends a program's input; pressing
            // Enter after it would be read by the shell as an empty command.
            let ending = if line.ends_with('\x04') { "" } else { "\r" };
            if stdin
                .write_all(format!("{line}{ending}").as_bytes())
                .is_err()
            {
                failures.push(format!("could not send {line:?}"));
                break;
            }
            position = wait_for_prompt(from);
            let Some(to) = position else {
                failures.push(format!("no prompt after {line:?}"));
                break;
            };
            let produced = String::from_utf8_lossy(&output.lock().expect("output lock")[from..to])
                .replace("\r\n", "\n");
            if !produced.contains(expected) {
                failures.push(format!(
                    "{line:?}: expected {expected:?} in output {produced:?}"
                ));
            }
        }
        if let (Some(check), Some(from)) = (display, position) {
            // Type on the emulated PS/2 keyboard; the command's output on
            // the serial console proves the key presses arrived.
            match type_on_keyboard(&check.monitor, KEYBOARD_TEST_KEYS) {
                Ok(()) => match wait_for_prompt(from) {
                    Some(to) => {
                        let produced =
                            String::from_utf8_lossy(&output.lock().expect("output lock")[from..to])
                                .replace("\r\n", "\n");
                        if !produced.contains(KEYBOARD_TEST_OUTPUT) {
                            failures.push(format!(
                                "keyboard input: expected {KEYBOARD_TEST_OUTPUT:?} in {produced:?}"
                            ));
                        }
                    }
                    None => failures.push("no prompt after keyboard input".into()),
                },
                Err(error) => failures.push(format!("cannot type on the keyboard: {error}")),
            }
            match take_screenshot(&check.monitor, &check.screenshot) {
                Ok(image) => failures.extend(check_screenshot(&image)),
                Err(error) => failures.push(format!("cannot take a screenshot: {error}")),
            }
            if check.desktop && failures.is_empty() {
                // The desktop runs in the foreground: the prompt returns
                // when it has been closed.
                let from = output.lock().expect("output lock").len();
                let _ = stdin.write_all(b"desktop\r");
                if let Err(error) = desktop_scenario(check) {
                    failures.push(format!("desktop: {error}"));
                }
                let after_desktop = wait_for_prompt(from);
                if after_desktop.is_none() {
                    failures.push("desktop: no prompt after closing the desktop".into());
                }
                // What was typed into `tee` inside the desktop reached
                // the file through the program's standard input.
                // And what was chosen in Settings (the teal accent, last)
                // was written down for the next start of the desktop.
                let mut from = after_desktop;
                for (command, expected, failure) in [
                    (
                        "cat /typed",
                        "\nkeyboard input\n",
                        "the line typed into tee is not in its file",
                    ),
                    (
                        "ls /bin",
                        "stuff",
                        "the folder made in Files does not exist",
                    ),
                    (
                        "cat /note-1.txt",
                        "\nhello notes\nsecond\n",
                        "the note saved in Notes is not in its file",
                    ),
                    (
                        "cat /desktop.conf",
                        "\naccent=teal\n",
                        "the settings chosen were not kept",
                    ),
                ] {
                    let Some(start) = from else { break };
                    let _ = stdin.write_all(format!("{command}\r").as_bytes());
                    from = wait_for_prompt(start);
                    let shown = from.map(|to| {
                        String::from_utf8_lossy(&output.lock().expect("output lock")[start..to])
                            .replace("\r\n", "\n")
                    });
                    if !shown.is_some_and(|text| text.contains(expected)) {
                        failures.push(format!("desktop: {failure}"));
                    }
                }
                // The text console must be back on the screen. (The
                // screen is drawn after the serial line is written, so
                // what was just printed may take a moment to be there.)
                let mut seen = Err("no screenshot taken".to_string());
                for _ in 0..20 {
                    seen = match take_screenshot(&check.monitor, &check.screenshot) {
                        Ok(image) => Ok(check_screenshot(&image)),
                        Err(error) => Err(error.to_string()),
                    };
                    if seen.as_ref().is_ok_and(Vec::is_empty) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(500));
                }
                match seen {
                    Ok(wrong) => failures.extend(
                        wrong
                            .into_iter()
                            .map(|failure| format!("after the desktop: {failure}")),
                    ),
                    Err(error) => failures.push(format!("cannot take a screenshot: {error}")),
                }
            }
        }
        if !ends_with_reset {
            let _ = stdin.write_all(b"exit\r");
        }
    }
    // Keep stdin open until QEMU exits: closing it early makes QEMU's
    // serial backend see end-of-file.
    let stdin = stdin;

    // The reader finishes when QEMU closes its stdout, i.e. when it exits.
    while !reader.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let timed_out = !reader.is_finished();
    if timed_out {
        let _ = child.kill();
    }
    let status = child.wait()?;
    drop(stdin);
    reader.join().expect("reader thread");
    let serial = String::from_utf8_lossy(&output.lock().expect("output lock")).into_owned();
    let stderr = stderr_reader.join().expect("stderr thread");
    if !stderr.trim().is_empty() {
        eprintln!("xtask: qemu stderr: {}", stderr.trim());
    }

    Ok(QemuRun {
        exit: if timed_out { None } else { status.code() },
        serial,
        elapsed: start.elapsed(),
    })
}

/// Fills a directory with the files QEMU's TFTP server offers to the
/// `network` test and returns it.
fn prepare_tftp_files(dir: &Path, options: &Options) -> Result<PathBuf> {
    let files = dir.join("tftp");
    fs::create_dir_all(&files)?;
    let pattern = |length: usize| -> Vec<u8> { (0..length).map(|i| (i % 251) as u8).collect() };
    fs::write(files.join("short.txt"), pattern(300))?;
    fs::write(files.join("exact.bin"), pattern(1024))?;
    let programs = workspace_root()
        .join("target")
        .join("x86_64-unknown-none")
        .join(options.profile_dir());
    for program in ["desktop", "hello"] {
        fs::copy(programs.join(program), files.join(program))?;
    }
    let _ = fs::remove_file(files.join("missing.txt"));
    Ok(files)
}

/// UDP port, on the host's loopback address, of the name server the
/// `network` test asks.
const TEST_NAME_SERVER_PORT: u16 = 15353;

/// Every port on the host that the `network` tests use or name, as the
/// tests are written. Each can be moved by the same amount with the
/// environment variable `OMIT_TEST_PORT_OFFSET`, so that two runs on
/// one machine do not take each other's ports.
const TEST_PORTS: [u16; 6] = [
    TEST_NAME_SERVER_PORT,
    // Where the tests expect nobody: a name server and a web server
    // that are not there.
    TEST_NAME_SERVER_PORT + 1,
    TEST_WEB_SERVER_PORT,
    TEST_WEB_SERVER_PORT + 1,
    TEST_SECURE_WEB_SERVER_PORT,
    TEST_ECHO_HOST_PORT,
];

/// The amount by which this run's ports are moved.
fn test_port_offset() -> u16 {
    std::env::var("OMIT_TEST_PORT_OFFSET")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

/// The port this run uses for `port` of the tests as written.
fn test_port(port: u16) -> u16 {
    port + test_port_offset()
}

/// `text` (a command of a test, or what it expects) with the tests'
/// port numbers replaced by this run's.
fn with_test_ports(text: &str) -> String {
    let offset = test_port_offset();
    let mut text = text.to_string();
    if offset != 0 {
        for port in TEST_PORTS {
            text = text.replace(&port.to_string(), &(port + offset).to_string());
        }
    }
    text
}

/// Starts, once, a name server that knows one name: `test.omit.example`
/// is 192.0.2.7; any other name does not exist. It runs until xtask
/// exits.
fn start_test_name_server() -> Result<()> {
    use std::net::UdpSocket;
    use std::sync::OnceLock;
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.get().is_some() {
        return Ok(());
    }
    let port = test_port(TEST_NAME_SERVER_PORT);
    let socket = UdpSocket::bind(("127.0.0.1", port)).map_err(|error| {
        format!("cannot listen on UDP port {port} for the network test: {error}")
    })?;
    let _ = STARTED.set(());
    std::thread::spawn(move || {
        // "test.omit.example" as a question sees it.
        const KNOWN: &[u8] = b"\x04test\x04omit\x07example\x00";
        let mut query = [0u8; 512];
        while let Ok((length, from)) = socket.recv_from(&mut query) {
            // A header, a name, type and class.
            if length < 12 + 1 + 4 {
                continue;
            }
            let mut answer = query[..length].to_vec();
            if query[12..length - 4].eq_ignore_ascii_case(KNOWN) {
                // Response, recursion available, no error; one answer:
                // the name (a pointer to the question), A, IN, 60 s.
                answer[2..4].copy_from_slice(&[0x81, 0x80]);
                answer[6..8].copy_from_slice(&[0, 1]);
                answer.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
                answer.extend_from_slice(&[192, 0, 2, 7]);
            } else {
                // Name error.
                answer[2..4].copy_from_slice(&[0x81, 0x83]);
            }
            let _ = socket.send_to(&answer, from);
        }
    });
    Ok(())
}

/// TCP port, on the host's loopback address, of the web server the
/// `network` test fetches from.
const TEST_WEB_SERVER_PORT: u16 = 18080;
/// TCP port on the host that QEMU forwards to port 7 of the guest, where
/// the `network` test runs an echo server.
const TEST_ECHO_HOST_PORT: u16 = 17007;
/// Bytes the test sends through the guest's echo server.
const TEST_ECHO_LEN: usize = 20_000;

/// The directory the test web server serves `/desktop` from.
static WEB_FILES: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Whether the echo client has had its data back from the guest intact
/// since it was last started.
static ECHO_SUCCEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Starts, once, a small web server for the `network` test. It answers
/// one request per connection and closes, as the guest's HTTP/1.0
/// requests ask.
fn start_test_web_server(files: &Path) -> Result<()> {
    use std::net::TcpListener;
    use std::sync::OnceLock;
    *WEB_FILES.lock().map_err(|_| "web server state")? = Some(files.to_path_buf());
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.get().is_some() {
        return Ok(());
    }
    let port = test_port(TEST_WEB_SERVER_PORT);
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|error| {
        format!("cannot listen on TCP port {port} for the network test: {error}")
    })?;
    let _ = STARTED.set(());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                answer_web_request(&mut stream);
            });
        }
    });
    Ok(())
}

/// Reads one request from `stream` and answers it. Returns the path
/// asked for, if a request arrived.
fn answer_web_request(stream: &mut (impl Read + Write)) -> Option<String> {
    {
        {
            {
                // The request head.
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && request.len() < 4096 {
                    match stream.read(&mut byte) {
                        Ok(1) => request.push(byte[0]),
                        _ => return None,
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let path = request.split(' ').nth(1).unwrap_or("");
                let with_length = |status: &str, body: &[u8]| {
                    let mut response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    response.extend_from_slice(body);
                    response
                };
                let response = match path {
                    "/hello.txt" => with_length("200 OK", b"Hello from the test server\n"),
                    "/desktop" => {
                        let file = WEB_FILES
                            .lock()
                            .ok()
                            .and_then(|files| files.clone())
                            .and_then(|files| fs::read(files.join("desktop")).ok());
                        match file {
                            Some(bytes) => with_length("200 OK", &bytes),
                            None => with_length("500 Internal Server Error", b""),
                        }
                    }
                    // Larger than the largest single allocation of the
                    // guest's kernel (4 MiB).
                    "/big" => {
                        let body: Vec<u8> = (0..6usize << 20).map(|i| (i % 249) as u8).collect();
                        with_length("200 OK", &body)
                    }
                    // No length: the body ends where the connection does.
                    "/nolength" => {
                        b"HTTP/1.0 200 OK\r\n\r\nno length given\n".to_vec()
                    }
                    // Claims more than it sends.
                    "/short" => {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nonly this".to_vec()
                    }
                    "/moved" => with_test_ports("HTTP/1.1 301 Moved Permanently\r\nLocation: http://10.0.2.2:18080/hello.txt\r\nContent-Length: 0\r\n\r\n").into_bytes(),
                    _ => with_length("404 Not Found", b"not here\n"),
                };
                let _ = stream.write_all(&response);
                Some(path.to_string())
            }
        }
    }
}

/// TCP port, on the host's loopback address, of the web server that
/// speaks TLS for the `network` test.
const TEST_SECURE_WEB_SERVER_PORT: u16 = 18443;

/// Makes, with the `openssl` command, a certificate authority and a
/// certificate signed by it for the address the guest knows the host
/// by. Both are valid around the fixed moment the tests' clock is set
/// to, whenever the test is run. The authority's certificate is put
/// among the files the guest can fetch over TFTP, as `ca.pem`.
fn make_test_certificates(dir: &Path, tftp_files: &Path) -> Result<PathBuf> {
    let papers = dir.join("tls");
    let _ = fs::remove_dir_all(&papers);
    fs::create_dir_all(&papers)?;
    let openssl = |arguments: &[&str]| -> Result<()> {
        let output = Command::new("openssl")
            .args(arguments)
            .current_dir(&papers)
            .output()
            .map_err(|error| format!("the network test needs the openssl command: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "openssl {}: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(())
    };
    fs::write(
        papers.join("ca.cnf"),
        "[ca]\ndefault_ca = this\n[this]\ndatabase = index.txt\nnew_certs_dir = .\n\
         serial = serial\npolicy = any\nunique_subject = no\n[any]\ncommonName = supplied\n\
         [authority]\nbasicConstraints = critical,CA:TRUE\nkeyUsage = critical,keyCertSign\n\
         [server]\nsubjectAltName = IP:10.0.2.2,DNS:test.omit.example\n\
         basicConstraints = CA:FALSE\nkeyUsage = digitalSignature\n\
         extendedKeyUsage = serverAuth\n",
    )?;
    fs::write(papers.join("index.txt"), "")?;
    fs::write(papers.join("serial"), "01\n")?;
    let key = [
        "genpkey",
        "-algorithm",
        "EC",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-out",
    ];
    let dates = [
        "-startdate",
        "20260101000000Z",
        "-enddate",
        "20361231000000Z",
    ];
    let sign = [
        "ca", "-batch", "-config", "ca.cnf", "-notext", "-md", "sha256", "-keyfile", "ca.key",
    ];
    openssl(&[&key[..], &["ca.key"]].concat())?;
    openssl(&[
        "req",
        "-new",
        "-key",
        "ca.key",
        "-subj",
        "/CN=Omit OS test authority",
        "-out",
        "ca.csr",
    ])?;
    openssl(
        &[
            &sign[..],
            &dates[..],
            &[
                "-selfsign",
                "-extensions",
                "authority",
                "-in",
                "ca.csr",
                "-out",
                "ca.pem",
            ],
        ]
        .concat(),
    )?;
    openssl(&[&key[..], &["server.key"]].concat())?;
    openssl(&[
        "req",
        "-new",
        "-key",
        "server.key",
        "-subj",
        "/CN=Omit OS test server",
        "-out",
        "server.csr",
    ])?;
    openssl(
        &[
            &sign[..],
            &dates[..],
            &[
                "-cert",
                "ca.pem",
                "-extensions",
                "server",
                "-in",
                "server.csr",
                "-out",
                "server.pem",
            ],
        ]
        .concat(),
    )?;
    fs::copy(papers.join("ca.pem"), tftp_files.join("ca.pem"))?;
    Ok(papers)
}

/// The certificate and key the secure test web server shows, replaced
/// at every run of the `network` test.
static WEB_PAPERS: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Starts, once, the web server of [`start_test_web_server`] again
/// behind TLS (rustls, the library the guest's client is built on, with
/// the certificate from [`make_test_certificates`]). It ends each
/// answer properly, except that of `/short`, after which it just drops
/// the connection.
fn start_test_secure_web_server(papers: &Path) -> Result<()> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::net::TcpListener;
    use std::sync::{Arc, OnceLock};
    *WEB_PAPERS.lock().map_err(|_| "web server state")? = Some(papers.to_path_buf());
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.get().is_some() {
        return Ok(());
    }
    let listener = TcpListener::bind(("127.0.0.1", test_port(TEST_SECURE_WEB_SERVER_PORT)))
        .map_err(|error| {
            format!(
                "cannot listen on TCP port {} for the network test: {error}",
                test_port(TEST_SECURE_WEB_SERVER_PORT)
            )
        })?;
    let _ = STARTED.set(());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                // The papers of the run in progress.
                let Some(papers) = WEB_PAPERS.lock().ok().and_then(|papers| papers.clone()) else {
                    return;
                };
                let (Ok(certificates), Ok(key)) = (
                    CertificateDer::pem_file_iter(papers.join("server.pem"))
                        .and_then(Iterator::collect::<std::result::Result<Vec<_>, _>>),
                    PrivateKeyDer::from_pem_file(papers.join("server.key")),
                ) else {
                    return;
                };
                let Some(config) = rustls::ServerConfig::builder_with_provider(Arc::new(
                    rustls_rustcrypto::provider(),
                ))
                .with_safe_default_protocol_versions()
                .ok()
                .and_then(|builder| {
                    builder
                        .with_no_client_auth()
                        .with_single_cert(certificates, key)
                        .ok()
                }) else {
                    return;
                };
                let Ok(mut connection) = rustls::ServerConnection::new(Arc::new(config)) else {
                    return;
                };
                let mut secure = rustls::Stream::new(&mut connection, &mut stream);
                let path = answer_web_request(&mut secure);
                if path.as_deref() != Some("/short") {
                    secure.conn.send_close_notify();
                }
                let _ = secure.flush();
            });
        }
    });
    Ok(())
}

/// Starts a thread that keeps trying, for two minutes, to send
/// [`TEST_ECHO_LEN`] bytes through the echo server the `network` test
/// starts in the guest, and to get them back unchanged. Until the guest
/// listens, QEMU accepts the connection and then drops it.
fn start_test_echo_client() {
    use std::net::{Shutdown, TcpStream};
    use std::sync::atomic::Ordering;
    ECHO_SUCCEEDED.store(false, Ordering::SeqCst);
    thread::spawn(|| {
        let data: Vec<u8> = (0..TEST_ECHO_LEN).map(|i| (i % 253) as u8).collect();
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline && !ECHO_SUCCEEDED.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(300));
            let Ok(mut stream) = TcpStream::connect(("127.0.0.1", test_port(TEST_ECHO_HOST_PORT)))
            else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
            // Writing and reading at the same time, as the echo comes
            // back while more is still being sent.
            let Ok(mut reader) = stream.try_clone() else {
                continue;
            };
            let echoed = thread::spawn(move || {
                let mut echoed = Vec::new();
                let _ = reader.read_to_end(&mut echoed);
                echoed
            });
            let sent = stream.write_all(&data).is_ok();
            let _ = stream.shutdown(Shutdown::Write);
            let echoed = echoed.join().unwrap_or_default();
            if sent && echoed == data {
                ECHO_SUCCEEDED.store(true, Ordering::SeqCst);
            }
        }
    });
}

/// Runs one boot test; returns the list of failed expectations.
fn run_boot_test(
    test: &BootTest,
    artifacts: &Artifacts,
    options: &Options,
    timeout: Duration,
) -> Result<(Vec<String>, QemuRun)> {
    let dir = out_dir(options)?;
    let image = dir.join(format!("test-{}.img", test.name));
    let vars = dir.join(format!("test-{}.vars.fd", test.name));
    let log = dir.join(format!("test-{}.serial.log", test.name));
    let data_disk = dir.join(format!("test-{}.data.img", test.name));
    create_data_disk(&data_disk)?;
    let nvme_disk = dir.join(format!("test-{}.nvme.img", test.name));
    let mut qemu_args: Vec<String> = test.qemu_args.iter().map(|s| (*s).into()).collect();
    if test.nvme {
        create_data_disk(&nvme_disk)?;
        qemu_args.extend([
            "-drive".into(),
            format!("if=none,id=nvme,format=raw,file={}", nvme_disk.display()),
            "-device".into(),
            "nvme,drive=nvme,serial=omit-test".into(),
        ]);
    }
    let sata_disk = dir.join(format!("test-{}.sata.img", test.name));
    if test.sata {
        create_data_disk(&sata_disk)?;
        qemu_args.extend([
            "-drive".into(),
            format!("if=none,id=sata,format=raw,file={}", sata_disk.display()),
            "-device".into(),
            "ide-hd,drive=sata,bus=ide.2".into(),
        ]);
    }
    if let Some(card) = test.network {
        let files = prepare_tftp_files(&dir, options)?;
        start_test_name_server()?;
        start_test_web_server(&files)?;
        start_test_secure_web_server(&make_test_certificates(&dir, &files)?)?;
        start_test_echo_client();
        qemu_args.extend([
            "-netdev".into(),
            format!(
                "user,id=net0,tftp={},hostfwd=tcp:127.0.0.1:{}-:7",
                files.display(),
                test_port(TEST_ECHO_HOST_PORT)
            ),
            "-device".into(),
            format!("{card},netdev=net0,mac=52:54:00:12:34:56"),
        ]);
    }
    let qemu_args: Vec<&str> = qemu_args.iter().map(String::as_str).collect();
    // The disk an installation is expected on.
    let target_disk = if test.nvme {
        &nvme_disk
    } else if test.sata {
        &sata_disk
    } else {
        &data_disk
    };

    // Tests without a script must not sit at the shell prompt forever.
    let shell = if test.script.is_empty() && !test.no_serial {
        "noshell "
    } else {
        ""
    };
    let cmdline = format!("qemu-exit {shell}{}", test.cmdline);
    build_media(artifacts, cmdline.trim(), &image, options)?;
    let mut failures = Vec::new();
    let display = test.display.then(|| DisplayCheck {
        monitor: dir.join(format!("test-{}.monitor", test.name)),
        screenshot: dir.join(format!("test-{}.screen.ppm", test.name)),
        desktop: test.desktop,
    });
    let command = qemu_command(
        Some(&image),
        &vars,
        &data_disk,
        display.as_ref().map(|d| d.monitor.as_path()),
        &qemu_args,
        options,
    )?;
    // The script, with this run's ports where it names any.
    let script: Vec<(String, String)> = test
        .script
        .iter()
        .map(|(command, expected)| (with_test_ports(command), with_test_ports(expected)))
        .collect();
    let script: Vec<(&str, &str)> = script
        .iter()
        .map(|(command, expected)| (command.as_str(), expected.as_str()))
        .collect();
    let run = match &display {
        Some(display) if test.no_serial => {
            run_without_serial(command, display, timeout, &mut failures)?
        }
        _ => run_qemu_captured(
            command,
            &script,
            test.ends_with_reset,
            display.as_ref(),
            timeout,
            &mut failures,
        )?,
    };
    if let Some(display) = &display {
        let _ = fs::remove_file(&display.monitor);
    }
    let mut run = run;
    if test.network.is_some() && !ECHO_SUCCEEDED.load(std::sync::atomic::Ordering::SeqCst) {
        failures.push(
            "the host did not get its data back unchanged from the guest's echo server".into(),
        );
    }
    if !test.reboot_script.is_empty() {
        // Same machine, same data disk, fresh boot — from the boot medium
        // again, or from the data disk if the system was installed on it.
        let medium = (!test.boot_installed).then_some(image.as_path());
        let second = run_qemu_captured(
            qemu_command(medium, &vars, &data_disk, None, &qemu_args, options)?,
            test.reboot_script,
            false,
            None,
            timeout,
            &mut failures,
        )?;
        if second.exit != Some(test.expected_exit) {
            failures.push(format!(
                "after reboot: QEMU exit status {:?}, expected {}",
                second.exit, test.expected_exit
            ));
        }
        run.serial.push_str("\n===== reboot =====\n");
        run.serial.push_str(&second.serial);
        run.elapsed += second.elapsed;
        if test.boot_installed {
            failures.extend(check_installed_disk(
                target_disk,
                artifacts,
                cmdline.trim(),
                &dir,
            ));
        } else {
            failures.extend(check_ext2(&data_disk));
        }
    }
    fs::write(&log, &run.serial)?;

    match run.exit {
        None => failures.push(format!("timed out after {}s", timeout.as_secs())),
        Some(code) if code != test.expected_exit => failures.push(format!(
            "QEMU exit status {code}, expected {}",
            test.expected_exit
        )),
        Some(_) => {}
    }
    for needle in test.must_contain {
        if !run.serial.contains(needle) {
            failures.push(format!("serial log lacks {needle:?}"));
        }
    }
    for needle in test.must_not_contain {
        if run.serial.contains(needle) {
            failures.push(format!("serial log contains forbidden {needle:?}"));
        }
    }
    // The success line must be a line of its own, not part of another
    // message.
    if test.must_contain.contains(&SUCCESS_MESSAGE)
        && !run.serial.lines().any(|l| l.trim_end() == SUCCESS_MESSAGE)
    {
        failures.push("success message is not on a line of its own".into());
    }
    let _ = fs::remove_file(&image);
    let _ = fs::remove_file(&vars);
    let _ = fs::remove_file(&data_disk);
    let _ = fs::remove_file(&nvme_disk);
    let _ = fs::remove_file(&sata_disk);
    Ok((failures, run))
}

/// Runs the QEMU integration tests, reporting in TAP format.
fn cmd_test(options: &Options) -> Result<ExitCode> {
    let artifacts = build(options)?;
    let timeout = Duration::from_secs(
        std::env::var("OMIT_TEST_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120),
    );

    let tests: Vec<&BootTest> = BOOT_TESTS
        .iter()
        .filter(|test| options.only.as_deref().is_none_or(|name| name == test.name))
        .collect();
    if tests.is_empty() {
        return Err("no boot test has that name".into());
    }

    println!("TAP version 13");
    println!("1..{}", tests.len());
    let mut failed = 0;
    for (index, test) in tests.iter().enumerate() {
        // The desktop tests operate a dozen windows by mouse and
        // keyboard, each step waiting for the screen: three times the
        // time of the others.
        let timeout = if test.desktop { timeout * 3 } else { timeout };
        let (failures, run) = run_boot_test(test, &artifacts, options, timeout)?;
        let number = index + 1;
        if failures.is_empty() {
            println!(
                "ok {number} - {} # {:.1}s",
                test.name,
                run.elapsed.as_secs_f64()
            );
        } else {
            failed += 1;
            println!("not ok {number} - {}", test.name);
            for failure in &failures {
                println!("#   {failure}");
            }
            println!("#   --- serial log ---");
            for line in run.serial.lines() {
                println!("#   {line}");
            }
        }
    }
    println!(
        "# {} profile, accel={}, cpu={}: {} passed, {failed} failed; serial logs in {}",
        options.profile_dir(),
        options.accel.as_deref().unwrap_or("tcg"),
        options.cpu.as_deref().unwrap_or("max"),
        tests.len() - failed,
        out_dir(options)?.display()
    );
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let result = Options::parse(rest).and_then(|options| match command.as_str() {
        "build" => build(&options).map(|_| ExitCode::SUCCESS),
        "image" => cmd_image(&options).map(|image| {
            println!("{}", image.display());
            ExitCode::SUCCESS
        }),
        "dist" => cmd_dist(&options).map(|()| ExitCode::SUCCESS),
        "run" => cmd_run(&options),
        "test" => cmd_test(&options),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}").into()),
    });

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xtask: error: {error}");
            ExitCode::FAILURE
        }
    }
}
