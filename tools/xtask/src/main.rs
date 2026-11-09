// SPDX-License-Identifier: GPL-2.0-only
//! `cargo xtask` — the Omit OS build, image, run and test driver.
//!
//! Everything that goes beyond a plain `cargo build` lives here, in Rust,
//! so that building and testing the OS needs no shell scripts, Makefiles or
//! host tools other than Cargo and QEMU. Run `cargo xtask help` for usage.

mod gpt;

use std::error::Error;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "\
Omit OS build driver

USAGE:
    cargo xtask <COMMAND> [OPTIONS]

COMMANDS:
    build    Build the boot loader and the kernel
    image    Build a bootable UEFI disk image
    run      Build and boot the image in QEMU (serial console on stdio)
    test     Build and run the QEMU integration tests
    help     Show this message

OPTIONS:
    --release           Build with optimisations
    --cmdline <TEXT>    Kernel command line to embed in the image (image, run)
    --cpu <MODEL>       QEMU CPU model (run, test) [default: max]
    --memory <SIZE>     QEMU guest memory (run, test) [default: 256M]
    --accel <NAME>      QEMU accelerator, e.g. kvm (run, test) [default: tcg]
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
    let target = workspace_root().join("target");
    Ok(Artifacts {
        loader: target
            .join("x86_64-unknown-uefi")
            .join(options.profile_dir())
            .join("omit-boot.efi"),
        kernel: target
            .join("x86_64-unknown-none")
            .join(options.profile_dir())
            .join("omit-kernel"),
    })
}

/// Builds a GPT disk image whose EFI System Partition contains the loader,
/// the kernel and (if non-empty) the kernel command line.
///
/// The image is reproducible: partition and volume identifiers are fixed and
/// no timestamps are recorded.
fn build_image(artifacts: &Artifacts, cmdline: &str, path: &Path) -> Result<()> {
    if !cmdline.is_ascii() || cmdline.len() > 1024 {
        return Err("kernel command line must be ASCII and at most 1024 bytes".into());
    }

    let mut disk = vec![0u8; IMAGE_SIZE as usize];
    gpt::write_partition_table(&mut disk);
    let esp = gpt::esp_extent(IMAGE_SIZE);
    let esp_bytes = &mut disk[esp.offset as usize..(esp.offset + esp.size) as usize];

    let mut volume = Cursor::new(esp_bytes);
    fatfs::format_volume(
        &mut volume,
        fatfs::FormatVolumeOptions::new()
            .fat_type(fatfs::FatType::Fat32)
            .volume_id(0x4F4D_4954) // "OMIT"
            .volume_label(*b"OMIT OS    "),
    )?;
    {
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
        if !cmdline.is_empty() {
            put(&omit, "CMDLINE", cmdline.as_bytes())?;
        }
    }

    fs::write(path, &disk)?;
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
fn qemu_command(image: &Path, vars: &Path, options: &Options) -> Result<Command> {
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
        .arg(format!("format=raw,file={}", image.display()))
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        .args(["-serial", "stdio"])
        .args(["-display", "none"])
        .args(["-monitor", "none"])
        .args(["-nic", "none"])
        .arg("-no-reboot");
    Ok(command)
}

fn cmd_image(options: &Options) -> Result<PathBuf> {
    let artifacts = build(options)?;
    let image = out_dir(options)?.join("omit-os.img");
    build_image(&artifacts, &options.cmdline, &image)?;
    Ok(image)
}

fn cmd_run(options: &Options) -> Result<ExitCode> {
    let image = cmd_image(options)?;
    let vars = out_dir(options)?.join("ovmf-vars.fd");
    let mut command = qemu_command(&image, &vars, options)?;
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
            "[INFO ] irq: legacy PIC masked; local APIC 0 enabled",
            "[INFO ] time: timer interrupt self-test passed",
            "[INFO ] paging: audit passed (W^X holds)",
        ],
        must_not_contain: &["KERNEL PANIC", "KERNEL EXCEPTION", "[ERROR]", "[WARN ]"],
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
    },
];

/// Outcome of running QEMU to completion.
struct QemuRun {
    /// Exit status, or `None` if QEMU was killed after the timeout.
    exit: Option<i32>,
    serial: String,
    elapsed: Duration,
}

fn run_qemu_captured(mut command: Command, timeout: Duration) -> Result<QemuRun> {
    let start = Instant::now();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start QEMU: {e}"))?;

    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut serial = Vec::new();
        let _ = stdout.read_to_end(&mut serial);
        let _ = tx.send(());
        serial
    });
    let stderr_reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    // The reader finishes when QEMU closes its stdout, i.e. when it exits.
    let timed_out = rx.recv_timeout(timeout).is_err();
    if timed_out {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let serial = String::from_utf8_lossy(&reader.join().expect("reader thread")).into_owned();
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

    let cmdline = format!("qemu-exit {}", test.cmdline);
    build_image(artifacts, cmdline.trim(), &image)?;
    let run = run_qemu_captured(qemu_command(&image, &vars, options)?, timeout)?;
    fs::write(&log, &run.serial)?;

    let mut failures = Vec::new();
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
