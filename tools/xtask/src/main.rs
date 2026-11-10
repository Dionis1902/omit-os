// SPDX-License-Identifier: GPL-2.0-only
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
    --iso               Use a bootable ISO instead of a disk image (image, run, test)
    --gdb               Start QEMU paused with a GDB server on :1234 (run)

ENVIRONMENT:
    QEMU                 QEMU binary [default: qemu-system-x86_64]
    OVMF_CODE, OVMF_VARS UEFI firmware images (auto-detected if unset)
    OMIT_TEST_TIMEOUT    Per-test timeout in seconds [default: 120]
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
    kernel: PathBuf,
    /// The initial ramdisk: a tar archive of the user programs.
    initrd: Vec<u8>,
}

/// User programs shipped in `/bin` of the initial ramdisk.
const USER_PROGRAMS: &[&str] = &["crash", "hello", "sleeper", "spin"];

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
    Ok(Artifacts {
        loader: target
            .join("x86_64-unknown-uefi")
            .join(options.profile_dir())
            .join("omit-boot.efi"),
        kernel: target
            .join("x86_64-unknown-none")
            .join(options.profile_dir())
            .join("omit-kernel"),
        initrd: build_tar(&files)?,
    })
}

/// Size of the FAT boot image embedded in the ISO.
const ISO_BOOT_IMAGE_SIZE: usize = 24 << 20;

/// Text of the `README.TXT` placed on the ISO.
const ISO_README: &str = "\
Omit OS live image.

This disc boots on x86-64 machines with UEFI firmware (Secure Boot off).
It runs entirely from memory: nothing is installed and nothing on the
machine is modified. See https://github.com/dionis1902/omit-os
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
    put(&omit, "KERNEL.ELF", &fs::read(&artifacts.kernel)?)?;
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
    image: &Path,
    vars: &Path,
    monitor: Option<&Path>,
    options: &Options,
) -> Result<Command> {
    let media = if options.iso {
        format!(
            "format=raw,media=cdrom,readonly=on,file={}",
            image.display()
        )
    } else {
        format!("format=raw,file={}", image.display())
    };
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
        .arg("-drive")
        .arg(media)
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        .args(["-serial", "stdio"])
        .args(["-display", "none"])
        .args(["-nic", "none"])
        .arg("-no-reboot");
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
    let mut command = qemu_command(&image, &vars, None, options)?;
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
    /// Also test the screen and keyboard: after the script, type a command
    /// on the emulated PS/2 keyboard, check that it runs, then take a
    /// screenshot and check what the framebuffer console drew.
    display: bool,
}

const BOOT_TESTS: &[BootTest] = &[
    BootTest {
        name: "boot",
        cmdline: "",
        expected_exit: QEMU_EXIT_SUCCESS,
        must_contain: &[
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
            "[INFO ] paging: audit passed (W^X holds)",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
        script: &[],
        display: false,
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
            ("uname -a", "\nOmit OS 0.1.0 x86_64\n"),
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
            ("ls /bin", "\ncrash\nhello\nsleeper\nspin\n"),
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
            // Line editing: the typo is erased with backspace before Enter.
            ("echo line-edix\x7fting", "\nline-editing\n"),
        ],
        display: false,
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
        display: true,
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
        display: false,
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
        display: false,
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
        display: false,
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
        display: false,
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
}

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
    if image.count(text.clone(), SCREEN_FOREGROUND) < 500 {
        failures.push("text area shows no text".into());
    }
    if image.count(text, SCREEN_STATUS_BACKGROUND) != 0 {
        failures.push("status bar colour leaked into the text area".into());
    }
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
fn run_qemu_captured(
    mut command: Command,
    script: &[(&str, &str)],
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
            if stdin.write_all(format!("{line}\r").as_bytes()).is_err() {
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
        }
        let _ = stdin.write_all(b"exit\r");
    }
    drop(stdin);

    // The reader finishes when QEMU closes its stdout, i.e. when it exits.
    while !reader.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let timed_out = !reader.is_finished();
    if timed_out {
        let _ = child.kill();
    }
    let status = child.wait()?;
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

    // Tests without a script must not sit at the shell prompt forever.
    let shell = if test.script.is_empty() {
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
    });
    let run = run_qemu_captured(
        qemu_command(
            &image,
            &vars,
            display.as_ref().map(|d| d.monitor.as_path()),
            options,
        )?,
        test.script,
        display.as_ref(),
        timeout,
        &mut failures,
    )?;
    if let Some(display) = &display {
        let _ = fs::remove_file(&display.monitor);
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

    println!("TAP version 13");
    println!("1..{}", BOOT_TESTS.len());
    let mut failed = 0;
    for (index, test) in BOOT_TESTS.iter().enumerate() {
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
        BOOT_TESTS.len() - failed,
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
