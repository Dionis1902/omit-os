// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! The Omit OS built-in shell.
//!
//! A small command interpreter whose commands are implemented in-process on
//! top of the virtual filesystem. It exists so the system is usable before
//! there are processes and a userspace; once programs can be executed, the
//! file utilities here are expected to be replaced by real executables.
//!
//! The crate is `no_std` + `alloc`, has no `unsafe` code and knows nothing
//! about the kernel: input arrives as bytes through [`LineEditor`], output
//! goes to any [`core::fmt::Write`], and the filesystem is an
//! [`omit_vfs::Vfs`]. That makes every command unit-testable on the host.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod line;
pub mod parse;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};

use alloc::boxed::Box;
use omit_vfs::{FileSystem, FileType, FsError, Vfs, WriteMode, path};

pub use line::LineEditor;

/// Identification strings reported by `uname`.
#[derive(Debug, Clone, Copy)]
pub struct SystemInfo {
    /// Operating system name.
    pub sysname: &'static str,
    /// Kernel release.
    pub release: &'static str,
    /// Hardware architecture.
    pub machine: &'static str,
}

/// One line of `ps` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    /// Kernel thread ID.
    pub thread: u64,
    /// Process ID, for threads that run a user program.
    pub pid: Option<u64>,
    /// Scheduler state, e.g. `running`.
    pub state: &'static str,
    /// CPU time consumed, in milliseconds.
    pub cpu_ms: u64,
    /// Thread or program name.
    pub name: String,
}

/// What the shell needs from the system it runs on, beyond the filesystem.
pub trait Host {
    /// Starts the program whose executable image is `image` as a new
    /// process called `name`, with arguments `args` (the first being the
    /// program name as typed) and working directory `cwd`; returns its
    /// process ID.
    ///
    /// With `stdout`, the program's standard output goes to the existing
    /// file at that (normalised absolute) path, starting at its end if the
    /// flag is set and at its beginning otherwise.
    fn spawn(
        &mut self,
        name: &str,
        image: &[u8],
        args: &[String],
        cwd: &str,
        stdout: Option<(&str, bool)>,
    ) -> Result<u64, String>;

    /// Starts the programs in `stages` as a pipeline: each one's standard
    /// output is the next one's standard input. With `stdin`, the first
    /// reads from that existing file (a normalised absolute path);
    /// `stdout` redirects the last as for [`Host::spawn`]. Returns the
    /// process IDs in order. If any program cannot be started, none is
    /// left running.
    fn spawn_pipeline(
        &mut self,
        stages: &[Stage<'_>],
        cwd: &str,
        stdin: Option<&str>,
        stdout: Option<(&str, bool)>,
    ) -> Result<Vec<u64>, String>;

    /// Requests termination of process `pid`; `false` if it does not exist.
    fn kill(&mut self, pid: u64) -> bool;

    /// Lists all threads and processes.
    fn tasks(&mut self) -> Vec<TaskInfo>;

    /// Current memory usage.
    fn memory(&mut self) -> MemoryInfo;

    /// The date and time in seconds since 1970-01-01 00:00:00 UTC, if
    /// the machine has a clock.
    fn now(&mut self) -> Option<i64>;

    /// One line of description per PCI device.
    fn pci_devices(&mut self) -> Vec<String>;

    /// Names and sizes in bytes of the block devices.
    fn block_devices(&mut self) -> Vec<(String, u64)>;

    /// Creates an empty filesystem on block device `device`, destroying
    /// its contents.
    fn format_device(&mut self, device: &str) -> Result<(), String>;

    /// One line per driver-to-device binding.
    fn drivers(&mut self) -> Vec<String>;

    /// Detaches the driver from the device at `device`
    /// (`bus:device.function`).
    fn unbind_driver(&mut self, device: &str) -> Result<(), String>;

    /// Installs the running system onto the whole disk `device`, erasing
    /// it. `loader` and `kernel` are the contents of the boot loader and
    /// kernel files to put on it. Returns lines describing the result.
    fn install(
        &mut self,
        device: &str,
        loader: &[u8],
        kernel: &[u8],
    ) -> Result<Vec<String>, String>;

    /// Attributes that the driver of `device` exposes. Without `name`,
    /// lists them (one line each); with `name`, returns the attribute's
    /// value; with `value` as well, sets it and returns nothing.
    fn device_attribute(
        &mut self,
        device: &str,
        name: Option<&str>,
        value: Option<&str>,
    ) -> Result<Vec<String>, String>;

    /// Runs a watchdog subcommand and returns what to print.
    fn watchdog(&mut self, args: &[String]) -> Result<String, String>;

    /// Opens the filesystem on block device `device` for mounting.
    fn open_filesystem(&mut self, device: &str) -> Result<Box<dyn FileSystem>, String>;

    /// Lines describing the network interface.
    fn network(&mut self) -> Vec<String>;

    /// Sends echo request number `sequence` to `address` and waits for
    /// the reply.
    fn ping(&mut self, address: omit_net::Ipv4Addr, sequence: u16) -> Result<PingReply, String>;
}

/// An answer to an echo request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingReply {
    /// How long the answer took, in microseconds.
    pub micros: u64,
    /// Hops the reply had left when it arrived.
    pub ttl: u8,
    /// Bytes of data that came back.
    pub bytes: usize,
}

/// Memory usage figures for `free`, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryInfo {
    /// Physical memory managed by the kernel.
    pub total_kib: u64,
    /// Physical memory currently unallocated.
    pub free_kib: u64,
    /// Bytes handed out by the kernel heap, in KiB.
    pub kernel_heap_kib: u64,
}

/// What the caller should do after a command has run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Read the next command.
    Continue,
    /// The user asked to leave the shell.
    Exit,
    /// A program was started in the foreground. The caller must wait for
    /// process `pid` to end — without holding the filesystem, which the
    /// program may need — and then call [`Shell::finish_wait`].
    Wait {
        /// The process to wait for.
        pid: u64,
    },
}

/// A command failure: the message is printed as `command: message`.
struct Failure(String);

impl From<fmt::Error> for Failure {
    fn from(_: fmt::Error) -> Self {
        Self("output error".into())
    }
}

type CommandResult = Result<(), Failure>;

/// Names handled by the shell itself (`exit` is handled before dispatch).
const BUILTINS: &[&str] = &[
    "help", "echo", "pwd", "cd", "ls", "cat", "mkdir", "touch", "cp", "mv", "rm", "uname", "clear",
    "ps", "kill", "free", "lspci", "lsblk", "mkfs", "mount", "umount", "lsdrv", "unbind",
    "devattr", "watchdog", "install", "cmp", "date", "ifconfig", "ping",
];

/// Internal marker returned by `run` for names that are not built-ins.
/// A program found and read by the shell.
struct Program {
    display_name: String,
    image: Vec<u8>,
    argv: Vec<String>,
}

/// One program of a pipeline, ready to start.
#[derive(Debug, Clone, Copy)]
pub struct Stage<'a> {
    /// Name to show for the process.
    pub name: &'a str,
    /// The executable file's contents.
    pub image: &'a [u8],
    /// Arguments, starting with the name the program was invoked by.
    pub args: &'a [String],
}

/// Where the running system keeps the boot loader and kernel that the
/// installer copies.
pub const BOOT_LOADER_FILE: &str = "/boot/BOOTX64.EFI";
/// See [`BOOT_LOADER_FILE`].
pub const BOOT_KERNEL_FILE: &str = "/boot/KERNEL.ELF";

/// How a Windows (or DOS) executable starts.
pub const WINDOWS_MAGIC: &[u8] = b"MZ";
/// The program that runs Windows executables.
pub const WINDOWS_LOADER: &str = "/bin/winrun";

const NOT_BUILTIN: &str = "\0not-builtin";

/// Formats a filesystem error the conventional way: `path: reason`.
fn fs_failure(path: &str, error: FsError) -> Failure {
    Failure(format!("{path}: {error}"))
}

const HELP: &str = "\
Built-in commands:
  help                 show this text
  echo [-n] [TEXT]...  print arguments
  pwd                  print the current directory
  cd [DIR]             change the current directory (default: /)
  ls [-l] [PATH]...    list directory contents
  cat FILE...          print file contents
  mkdir [-p] DIR...    create directories
  touch FILE...        create empty files
  cp SOURCE DEST       copy a file
  cmp FILE1 FILE2      compare two files
  mv SOURCE DEST       move or rename a file or directory
  rm [-r] PATH...      remove files (-r: and directories, recursively)
  ps                   list threads and processes
  kill PID             terminate a process
  free                 show memory usage
  date                 show the date and time (UTC)
  ifconfig             show the network interface and its addresses
  ping ADDRESS [COUNT] send echo requests to an IPv4 address (default: 3)
  lspci                list PCI devices
  lsblk                list block devices
  lsdrv                list drivers and the devices they control
  unbind DEVICE        detach the driver from a device (e.g. 00:03.0)
  devattr DEVICE [NAME [VALUE]]
                       list, show or set the attributes a driver exposes
  install DEVICE --erase
                       install this system onto a disk, erasing it
  watchdog [COMMAND]   status, start, stop, ping, timeout SECONDS
  mkfs DEVICE          create an empty ext2 filesystem (erases the device)
  mount [DEVICE DIR]   mount a filesystem, or list what is mounted
  umount DIR           unmount a filesystem
  uname [-a]           print system information
  clear                clear the screen
  exit                 leave the shell
Anything else is run as a program from /bin (or by path); end the line with
'&' to run it in the background. Output of any command can be redirected
with '> FILE' or '>> FILE'. Programs can read from a file with '< FILE'
and be connected with '|': the output of one is the input of the next.
";

/// Shell state: the current directory and the last exit status.
#[derive(Debug)]
pub struct Shell {
    cwd: String,
    system: SystemInfo,
    last_status: i32,
    /// Name of the foreground program being waited for.
    foreground: Option<String>,
    /// Set by `run_program` when the caller has to wait for a process.
    pending_wait: Option<u64>,
    /// What is mounted at each mount point, in the VFS's order; the first
    /// is the root filesystem.
    mount_sources: Vec<String>,
}

impl Shell {
    /// Creates a shell whose current directory is `/`.
    pub fn new(system: SystemInfo) -> Self {
        Self {
            cwd: "/".into(),
            system,
            last_status: 0,
            foreground: None,
            pending_wait: None,
            mount_sources: alloc::vec!["ramfs".into()],
        }
    }

    /// The current directory, as a normalised absolute path.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// Exit status of the most recent command: 0 for success.
    pub fn last_status(&self) -> i32 {
        self.last_status
    }

    /// Writes the prompt.
    pub fn write_prompt(&self, out: &mut dyn Write) -> fmt::Result {
        write!(out, "omit:{}# ", self.cwd)
    }

    fn resolve(&self, arg: &str) -> Result<String, Failure> {
        path::normalize(&self.cwd, arg).map_err(|e| fs_failure(arg, e))
    }

    /// Parses and runs one line. Normal output and error messages are both
    /// written to `out` unless the line redirects output to a file, in which
    /// case only error messages reach `out`.
    pub fn execute(
        &mut self,
        line: &str,
        vfs: &mut Vfs,
        host: &mut dyn Host,
        out: &mut dyn Write,
    ) -> Outcome {
        let command = match parse::parse(line) {
            Ok(command) => command,
            Err(error) => {
                let _ = writeln!(out, "omsh: syntax error: {error}");
                self.last_status = 2;
                return Outcome::Continue;
            }
        };
        let Some((name, args)) = command.words.split_first() else {
            return Outcome::Continue;
        };
        if name == "exit" {
            return Outcome::Exit;
        }

        if !command.rest.is_empty() || command.input.is_some() {
            self.last_status = match self.run_pipeline(&command, vfs, host, out) {
                Ok(()) => 0,
                Err(Failure(message)) => {
                    let _ = writeln!(out, "{message}");
                    1
                }
            };
            return match self.pending_wait.take() {
                Some(pid) => Outcome::Wait { pid },
                None => Outcome::Continue,
            };
        }

        // With a redirection, standard output is collected and written to
        // the file afterwards; otherwise it streams straight to `out`.
        let is_builtin = BUILTINS.contains(&name.as_str());
        let result = if !is_builtin {
            // A program writes to its redirection target itself, so the
            // "[pid] name" line for a background job always goes to the
            // terminal.
            self.run_program(
                name,
                args,
                command.redirect.as_ref(),
                command.background,
                vfs,
                host,
                out,
            )
        } else if command.background {
            // `&` is meaningless for commands that run inside the shell.
            Err(Failure(
                "built-in commands cannot run in the background".into(),
            ))
        } else if let Some(redirect) = &command.redirect {
            // Output of a built-in is collected and written afterwards, so
            // a failing command leaves the target untouched.
            let mut captured = String::new();
            self.run(name, args, vfs, host, &mut captured)
                .and_then(|()| {
                    let target = self.resolve(&redirect.path)?;
                    let mode = if redirect.append {
                        WriteMode::Append
                    } else {
                        WriteMode::Truncate
                    };
                    vfs.write_file(&target, captured.as_bytes(), mode)
                        .map_err(|e| fs_failure(&redirect.path, e))
                })
        } else {
            self.run(name, args, vfs, host, out)
        };

        self.last_status = match result {
            Ok(()) => 0,
            Err(Failure(message)) => {
                let _ = writeln!(out, "{name}: {message}");
                1
            }
        };
        match self.pending_wait.take() {
            Some(pid) => Outcome::Wait { pid },
            None => Outcome::Continue,
        }
    }

    /// Completes a foreground program started by [`Shell::execute`]:
    /// records its exit status (`None` if the process could not be waited
    /// for) and reports a non-zero one.
    pub fn finish_wait(&mut self, status: Option<i32>, out: &mut dyn Write) {
        let name = self.foreground.take().unwrap_or_default();
        self.last_status = status.unwrap_or(0);
        if self.last_status != 0 {
            let _ = writeln!(out, "{name}: exited with status {}", self.last_status);
        }
    }

    fn run(
        &mut self,
        name: &str,
        args: &[String],
        vfs: &mut Vfs,
        host: &mut dyn Host,
        out: &mut dyn Write,
    ) -> CommandResult {
        match name {
            "help" => Ok(out.write_str(HELP)?),
            "echo" => self.echo(args, out),
            "pwd" => Ok(writeln!(out, "{}", self.cwd)?),
            "cd" => self.cd(args, vfs),
            "ls" => self.ls(args, vfs, out),
            "cat" => self.cat(args, vfs, out),
            "mkdir" => self.mkdir(args, vfs),
            "touch" => self.touch(args, vfs),
            "cp" => self.cp(args, vfs),
            "cmp" => self.cmp(args, vfs),
            "mv" => self.mv(args, vfs),
            "rm" => self.rm(args, vfs),
            "uname" => self.uname(args, out),
            "clear" => Ok(out.write_str("\x1b[2J\x1b[H")?),
            "ps" => self.ps(args, host, out),
            "kill" => self.kill(args, host),
            "lspci" => {
                for line in host.pci_devices() {
                    writeln!(out, "{line}")?;
                }
                Ok(())
            }
            "lsdrv" => {
                writeln!(out, "DEVICE   ORIGIN     DRIVER")?;
                for line in host.drivers() {
                    writeln!(out, "{line}")?;
                }
                Ok(())
            }
            "unbind" => {
                let [device] = args else {
                    return Err(Failure("usage: unbind DEVICE".into()));
                };
                host.unbind_driver(device)
                    .map_err(|error| Failure(format!("{device}: {error}")))
            }
            "install" => {
                // Destroying a disk's contents takes more than a typo.
                let [device, confirmation] = args else {
                    return Err(Failure("usage: install DEVICE --erase".into()));
                };
                if confirmation != "--erase" {
                    return Err(Failure(
                        "this erases the whole disk; confirm with: install DEVICE --erase".into(),
                    ));
                }
                if self
                    .mount_sources
                    .iter()
                    .any(|source| source.starts_with(device.as_str()))
                {
                    return Err(Failure(format!(
                        "{device}: a filesystem on this disk is mounted; unmount it first"
                    )));
                }
                // The boot loader and kernel are copied from the running
                // system.
                let read = |path: &str| {
                    vfs.read_file(path).map_err(|error| {
                        Failure(format!(
                            "cannot read {path}, which the installer copies: {error}"
                        ))
                    })
                };
                let loader = read(BOOT_LOADER_FILE)?;
                let kernel = read(BOOT_KERNEL_FILE)?;
                let lines = host
                    .install(device, &loader, &kernel)
                    .map_err(|error| Failure(format!("{device}: {error}")))?;
                for line in lines {
                    writeln!(out, "{line}")?;
                }
                Ok(())
            }
            "devattr" => {
                let (device, name, value) = match args {
                    [device] => (device, None, None),
                    [device, name] => (device, Some(name.as_str()), None),
                    [device, name, value] => (device, Some(name.as_str()), Some(value.as_str())),
                    _ => return Err(Failure("usage: devattr DEVICE [NAME [VALUE]]".into())),
                };
                let lines = host
                    .device_attribute(device, name, value)
                    .map_err(|error| Failure(format!("{device}: {error}")))?;
                for line in lines {
                    writeln!(out, "{line}")?;
                }
                Ok(())
            }
            "watchdog" => Ok(out.write_str(&host.watchdog(args).map_err(Failure)?)?),
            "lsblk" => {
                let devices = host.block_devices();
                let width = devices
                    .iter()
                    .map(|(name, _)| name.len())
                    .fold(6, usize::max);
                writeln!(out, "{:<width$}      SIZE", "NAME")?;
                for (device, bytes) in devices {
                    writeln!(out, "{device:<width$} {:>5} MiB", bytes >> 20)?;
                }
                Ok(())
            }
            "mkfs" => {
                let [device] = args else {
                    return Err(Failure("usage: mkfs DEVICE".into()));
                };
                host.format_device(device)
                    .map_err(|error| Failure(format!("{device}: {error}")))?;
                Ok(writeln!(
                    out,
                    "mkfs: created an ext2 filesystem on {device}"
                )?)
            }
            "mount" => match args {
                [] => {
                    for (point, source) in vfs.mount_points().iter().zip(&self.mount_sources) {
                        writeln!(out, "{source} on {point}")?;
                    }
                    Ok(())
                }
                [device, directory] => {
                    let target = self.resolve(directory)?;
                    let filesystem = host
                        .open_filesystem(device)
                        .map_err(|error| Failure(format!("{device}: {error}")))?;
                    vfs.mount(&target, filesystem)
                        .map_err(|e| fs_failure(directory, e))?;
                    self.mount_sources.push(device.clone());
                    Ok(())
                }
                _ => Err(Failure("usage: mount [DEVICE DIR]".into())),
            },
            "umount" => {
                let [directory] = args else {
                    return Err(Failure("usage: umount DIR".into()));
                };
                let target = self.resolve(directory)?;
                if target == self.cwd || path::is_inside(&self.cwd, &target) {
                    return Err(fs_failure(directory, FsError::Busy));
                }
                let index = vfs.mount_points().iter().position(|point| *point == target);
                // Dropping the filesystem releases its device.
                drop(vfs.unmount(&target).map_err(|e| fs_failure(directory, e))?);
                if let Some(index) = index {
                    self.mount_sources.remove(index);
                }
                Ok(())
            }
            _ if !BUILTINS.contains(&name) => Err(Failure(NOT_BUILTIN.into())),
            "ifconfig" => {
                if !args.is_empty() {
                    return Err(Failure("usage: ifconfig".into()));
                }
                for line in host.network() {
                    writeln!(out, "{line}")?;
                }
                Ok(())
            }
            "ping" => {
                const USAGE: &str = "usage: ping ADDRESS [COUNT]";
                let (target, count) = match args {
                    [target] => (target, 3),
                    [target, count] => match count.parse::<u16>() {
                        Ok(count @ 1..=100) => (target, count),
                        _ => return Err(Failure("COUNT must be between 1 and 100".into())),
                    },
                    _ => return Err(Failure(USAGE.into())),
                };
                let address = omit_net::Ipv4Addr::parse(target).ok_or_else(|| {
                    Failure(format!(
                        "{target}: not an IPv4 address (names cannot be looked up yet)"
                    ))
                })?;
                let mut received = 0;
                for sequence in 1..=count {
                    match host.ping(address, sequence) {
                        Ok(reply) => {
                            received += 1;
                            writeln!(
                                out,
                                "{} bytes from {address}: seq={sequence} ttl={} time={}.{:03} ms",
                                reply.bytes,
                                reply.ttl,
                                reply.micros / 1000,
                                reply.micros % 1000,
                            )?;
                        }
                        Err(error) => writeln!(out, "seq={sequence}: {error}")?,
                    }
                }
                writeln!(out, "{count} sent, {received} received")?;
                if received == 0 {
                    return Err(Failure(format!("no answer from {address}")));
                }
                Ok(())
            }
            "date" => {
                if !args.is_empty() {
                    return Err(Failure("usage: date".into()));
                }
                let moment = host
                    .now()
                    .and_then(omit_abi::datetime::DateTime::from_unix)
                    .ok_or_else(|| Failure("this machine has no usable clock".into()))?;
                const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
                Ok(writeln!(
                    out,
                    "{} {moment} UTC",
                    DAYS[usize::from(moment.weekday())]
                )?)
            }
            "free" => {
                let memory = host.memory();
                writeln!(out, "             total        used        free (KiB)")?;
                writeln!(
                    out,
                    "Memory: {:>10}  {:>10}  {:>10}",
                    memory.total_kib,
                    memory.total_kib - memory.free_kib,
                    memory.free_kib
                )?;
                Ok(writeln!(
                    out,
                    "Kernel heap in use: {} KiB",
                    memory.kernel_heap_kib
                )?)
            }
            _ => Err(Failure(NOT_BUILTIN.into())),
        }
    }

    /// Finds the program `name` (in `/bin` unless it contains a `/`), reads
    /// it, and builds its argument list.
    fn prepare_program(
        &self,
        name: &str,
        args: &[String],
        vfs: &mut Vfs,
    ) -> Result<Program, Failure> {
        let path = if name.contains('/') {
            self.resolve(name)?
        } else {
            path::join("/bin", name)
        };
        let image = match vfs.read_file(&path) {
            Ok(image) => image,
            Err(FsError::NotFound | FsError::NotADirectory) if !name.contains('/') => {
                return Err(Failure("command not found".into()));
            }
            Err(error) => return Err(Failure(format!("{error}"))),
        };
        let display_name = path::file_name(&path).unwrap_or(name).into();
        // By convention the first argument is the name the program was
        // invoked by.
        let mut argv = Vec::with_capacity(args.len() + 2);
        argv.push(String::from(name));
        // Windows executables are run by the Windows program loader, which
        // gets the file's path as its first argument.
        let image = if image.starts_with(WINDOWS_MAGIC) {
            argv[0] = WINDOWS_LOADER.into();
            argv.push(path.clone());
            vfs.read_file(WINDOWS_LOADER).map_err(|error| {
                Failure(format!(
                    "this is a Windows program, but {WINDOWS_LOADER} cannot be read: {error}"
                ))
            })?
        } else {
            image
        };
        argv.extend_from_slice(args);
        Ok(Program {
            display_name,
            image,
            argv,
        })
    }

    /// Prepares the target of an output redirection: it must exist, and
    /// `>` empties it, before a program starts writing. Returns its path
    /// and whether output is appended.
    fn prepare_output(
        &self,
        redirect: Option<&parse::Redirect>,
        vfs: &mut Vfs,
    ) -> Result<Option<(String, bool)>, Failure> {
        let Some(redirect) = redirect else {
            return Ok(None);
        };
        let target = self.resolve(&redirect.path)?;
        let mode = if redirect.append {
            WriteMode::Append
        } else {
            WriteMode::Truncate
        };
        vfs.write_file(&target, b"", mode)
            .map_err(|e| fs_failure(&redirect.path, e))?;
        Ok(Some((target, redirect.append)))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one call site; the arguments are the parsed command line"
    )]
    fn run_program(
        &mut self,
        name: &str,
        args: &[String],
        redirect: Option<&parse::Redirect>,
        background: bool,
        vfs: &mut Vfs,
        host: &mut dyn Host,
        out: &mut dyn Write,
    ) -> CommandResult {
        let program = self.prepare_program(name, args, vfs)?;
        let target = self.prepare_output(redirect, vfs)?;
        let stdout = target
            .as_ref()
            .map(|(path, append)| (path.as_str(), *append));
        let pid = host
            .spawn(
                &program.display_name,
                &program.image,
                &program.argv,
                &self.cwd,
                stdout,
            )
            .map_err(Failure)?;
        if background {
            writeln!(out, "[{pid}] {}", program.display_name)?;
        } else {
            self.foreground = Some(program.display_name);
            self.pending_wait = Some(pid);
        }
        Ok(())
    }

    /// Runs a pipeline, or a single program with redirected input. Every
    /// command must be a program: the shell's built-in commands do not
    /// take part (where a program of the same name exists in `/bin`, as
    /// for `cat`, `ls` and `echo`, that program is used). Error messages
    /// name the command they are about.
    fn run_pipeline(
        &mut self,
        command: &parse::CommandLine,
        vfs: &mut Vfs,
        host: &mut dyn Host,
        out: &mut dyn Write,
    ) -> CommandResult {
        let mut programs = Vec::with_capacity(1 + command.rest.len());
        for words in core::iter::once(&command.words).chain(&command.rest) {
            let Some((name, args)) = words.split_first() else {
                return Err(Failure("missing command".into()));
            };
            let program = self
                .prepare_program(name, args, vfs)
                .map_err(|Failure(message)| {
                    if BUILTINS.contains(&name.as_str()) {
                        Failure(format!(
                            "{name}: this built-in command cannot be used with '|' or '<'"
                        ))
                    } else {
                        Failure(format!("{name}: {message}"))
                    }
                })?;
            programs.push(program);
        }
        let stdin = match &command.input {
            Some(input) => {
                let source = self
                    .resolve(input)
                    .map_err(|Failure(message)| Failure(format!("{input}: {message}")))?;
                match vfs.metadata(&source) {
                    Ok(metadata) if metadata.file_type == FileType::Directory => {
                        return Err(Failure(format!("{input}: is a directory")));
                    }
                    Ok(_) => {}
                    Err(error) => return Err(fs_failure(input, error)),
                }
                Some(source)
            }
            None => None,
        };
        let target = self
            .prepare_output(command.redirect.as_ref(), vfs)
            .map_err(|Failure(message)| Failure(format!("omsh: {message}")))?;
        let stdout = target
            .as_ref()
            .map(|(path, append)| (path.as_str(), *append));

        let stages: Vec<Stage<'_>> = programs
            .iter()
            .map(|program| Stage {
                name: &program.display_name,
                image: &program.image,
                args: &program.argv,
            })
            .collect();
        let pids = host
            .spawn_pipeline(&stages, &self.cwd, stdin.as_deref(), stdout)
            .map_err(|message| Failure(format!("omsh: {message}")))?;
        // The pipeline's result is that of its last command; the others
        // are not waited for and are cleaned up when they end.
        let (Some(&last_pid), Some(last)) = (pids.last(), programs.pop()) else {
            return Err(Failure("omsh: no program was started".into()));
        };
        if command.background {
            writeln!(out, "[{last_pid}] {}", last.display_name)?;
        } else {
            self.foreground = Some(last.display_name);
            self.pending_wait = Some(last_pid);
        }
        Ok(())
    }

    fn ps(&self, args: &[String], host: &mut dyn Host, out: &mut dyn Write) -> CommandResult {
        if !args.is_empty() {
            return Err(Failure("extra operand".into()));
        }
        writeln!(out, "  PID   TID STATE     CPU(ms) NAME")?;
        for task in host.tasks() {
            match task.pid {
                Some(pid) => write!(out, "{pid:>5}")?,
                None => write!(out, "    -")?,
            }
            writeln!(
                out,
                " {:>5} {:<8} {:>8} {}",
                task.thread, task.state, task.cpu_ms, task.name
            )?;
        }
        Ok(())
    }

    fn kill(&self, args: &[String], host: &mut dyn Host) -> CommandResult {
        let [arg] = args else {
            return Err(Failure("usage: kill PID".into()));
        };
        let pid: u64 = arg
            .parse()
            .map_err(|_| Failure(format!("{arg}: not a process ID")))?;
        if host.kill(pid) {
            Ok(())
        } else {
            Err(Failure(format!("{pid}: no such process")))
        }
    }

    fn echo(&self, args: &[String], out: &mut dyn Write) -> CommandResult {
        let (newline, args) = match args.split_first() {
            Some((first, rest)) if first == "-n" => (false, rest),
            _ => (true, args),
        };
        for (index, arg) in args.iter().enumerate() {
            if index > 0 {
                out.write_char(' ')?;
            }
            out.write_str(arg)?;
        }
        if newline {
            out.write_char('\n')?;
        }
        Ok(())
    }

    fn cd(&mut self, args: &[String], vfs: &Vfs) -> CommandResult {
        let arg = match args {
            [] => "/",
            [dir] => dir.as_str(),
            _ => return Err(Failure("too many arguments".into())),
        };
        let target = self.resolve(arg)?;
        let metadata = vfs.metadata(&target).map_err(|e| fs_failure(arg, e))?;
        if metadata.file_type != FileType::Directory {
            return Err(fs_failure(arg, FsError::NotADirectory));
        }
        self.cwd = target;
        Ok(())
    }

    fn ls(&self, args: &[String], vfs: &Vfs, out: &mut dyn Write) -> CommandResult {
        let (flags, paths) = split_flags(args, "l")?;
        let long = flags.contains('l');
        let default = [String::from(".")];
        let paths = if paths.is_empty() {
            &default[..]
        } else {
            paths
        };

        let print = |out: &mut dyn Write, kind: FileType, size: u64, name: &str| {
            if long {
                let tag = match kind {
                    FileType::Directory => 'd',
                    FileType::File => '-',
                };
                writeln!(out, "{tag} {size:>10} {name}")
            } else {
                writeln!(out, "{name}")
            }
        };

        for (index, arg) in paths.iter().enumerate() {
            let target = self.resolve(arg)?;
            let metadata = vfs.metadata(&target).map_err(|e| fs_failure(arg, e))?;
            if metadata.file_type == FileType::File {
                print(out, FileType::File, metadata.size, arg)?;
                continue;
            }
            if paths.len() > 1 {
                if index > 0 {
                    out.write_char('\n')?;
                }
                writeln!(out, "{arg}:")?;
            }
            for entry in vfs.read_dir(&target).map_err(|e| fs_failure(arg, e))? {
                let size = vfs
                    .metadata(&path::join(&target, &entry.name))
                    .map(|m| m.size)
                    .unwrap_or(0);
                print(out, entry.file_type, size, &entry.name)?;
            }
        }
        Ok(())
    }

    fn cat(&self, args: &[String], vfs: &Vfs, out: &mut dyn Write) -> CommandResult {
        if args.is_empty() {
            return Err(Failure("missing file operand".into()));
        }
        for arg in args {
            let data = vfs
                .read_file(&self.resolve(arg)?)
                .map_err(|e| fs_failure(arg, e))?;
            out.write_str(&String::from_utf8_lossy(&data))?;
        }
        Ok(())
    }

    fn mkdir(&self, args: &[String], vfs: &mut Vfs) -> CommandResult {
        let (flags, paths) = split_flags(args, "p")?;
        if paths.is_empty() {
            return Err(Failure("missing operand".into()));
        }
        for arg in paths {
            let target = self.resolve(arg)?;
            if flags.contains('p') {
                vfs.create_dir_all(&target)
            } else {
                vfs.create_dir(&target)
            }
            .map_err(|e| fs_failure(arg, e))?;
        }
        Ok(())
    }

    fn touch(&self, args: &[String], vfs: &mut Vfs) -> CommandResult {
        if args.is_empty() {
            return Err(Failure("missing file operand".into()));
        }
        for arg in args {
            let target = self.resolve(arg)?;
            match vfs.metadata(&target) {
                // There are no timestamps to update on an existing file.
                Ok(_) => {}
                Err(FsError::NotFound) => vfs
                    .write_file(&target, b"", WriteMode::Truncate)
                    .map_err(|e| fs_failure(arg, e))?,
                Err(e) => return Err(fs_failure(arg, e)),
            }
        }
        Ok(())
    }

    /// Resolves the destination of `cp`/`mv`: if `dest` is an existing
    /// directory, the source keeps its name inside it.
    fn destination(&self, source: &str, dest_arg: &str, vfs: &Vfs) -> Result<String, Failure> {
        let dest = self.resolve(dest_arg)?;
        match vfs.metadata(&dest) {
            Ok(metadata) if metadata.file_type == FileType::Directory => {
                let name = path::file_name(source)
                    .ok_or_else(|| fs_failure(source, FsError::InvalidArgument))?;
                Ok(path::join(&dest, name))
            }
            _ => Ok(dest),
        }
    }

    fn cp(&self, args: &[String], vfs: &mut Vfs) -> CommandResult {
        let [source_arg, dest_arg] = args else {
            return Err(Failure("usage: cp SOURCE DEST".into()));
        };
        let source = self.resolve(source_arg)?;
        let dest = self.destination(&source, dest_arg, vfs)?;
        if source == dest {
            return Err(Failure(format!(
                "{source_arg} and {dest_arg} are the same file"
            )));
        }
        vfs.copy_file(&source, &dest).map_err(|e| match e {
            // Reading a directory as a file: name the source.
            FsError::IsADirectory if vfs.read_dir(&source).is_ok() => Failure(format!(
                "{source_arg}: Is a directory (recursive copy is not supported)"
            )),
            e => fs_failure(dest_arg, e),
        })
    }

    /// Compares two files byte by byte: silent if they are equal.
    fn cmp(&self, args: &[String], vfs: &mut Vfs) -> CommandResult {
        let [first_arg, second_arg] = args else {
            return Err(Failure("usage: cmp FILE1 FILE2".into()));
        };
        let first = vfs
            .read_file(&self.resolve(first_arg)?)
            .map_err(|e| fs_failure(first_arg, e))?;
        let second = vfs
            .read_file(&self.resolve(second_arg)?)
            .map_err(|e| fs_failure(second_arg, e))?;
        if let Some(at) = first.iter().zip(&second).position(|(a, b)| a != b) {
            return Err(Failure(format!(
                "{first_arg} and {second_arg} differ at byte {}",
                at + 1
            )));
        }
        if first.len() != second.len() {
            let shorter = if first.len() < second.len() {
                first_arg
            } else {
                second_arg
            };
            return Err(Failure(format!(
                "{shorter} ends after byte {}, the other file continues",
                first.len().min(second.len())
            )));
        }
        Ok(())
    }

    fn mv(&self, args: &[String], vfs: &mut Vfs) -> CommandResult {
        let [source_arg, dest_arg] = args else {
            return Err(Failure("usage: mv SOURCE DEST".into()));
        };
        let source = self.resolve(source_arg)?;
        let dest = self.destination(&source, dest_arg, vfs)?;
        if source == dest {
            return Err(Failure(format!(
                "{source_arg} and {dest_arg} are the same file"
            )));
        }
        vfs.metadata(&source)
            .map_err(|e| fs_failure(source_arg, e))?;
        vfs.rename(&source, &dest)
            .map_err(|e| fs_failure(dest_arg, e))
    }

    fn rm(&self, args: &[String], vfs: &mut Vfs) -> CommandResult {
        let (flags, paths) = split_flags(args, "r")?;
        if paths.is_empty() {
            return Err(Failure("missing operand".into()));
        }
        for arg in paths {
            let target = self.resolve(arg)?;
            let metadata = vfs.metadata(&target).map_err(|e| fs_failure(arg, e))?;
            if target == self.cwd || is_ancestor(&target, &self.cwd) {
                return Err(Failure(format!(
                    "{arg}: refusing to remove the current directory or its ancestor"
                )));
            }
            if flags.contains('r') {
                vfs.remove_all(&target)
            } else if metadata.file_type == FileType::Directory {
                Err(FsError::IsADirectory)
            } else {
                vfs.remove(&target)
            }
            .map_err(|e| fs_failure(arg, e))?;
        }
        Ok(())
    }

    fn uname(&self, args: &[String], out: &mut dyn Write) -> CommandResult {
        let (flags, rest) = split_flags(args, "asrm")?;
        if !rest.is_empty() {
            return Err(Failure("extra operand".into()));
        }
        let all = flags.contains('a');
        let mut fields = alloc::vec::Vec::new();
        if all || flags.contains('s') || flags.is_empty() {
            fields.push(self.system.sysname);
        }
        if all || flags.contains('r') {
            fields.push(self.system.release);
        }
        if all || flags.contains('m') {
            fields.push(self.system.machine);
        }
        Ok(writeln!(out, "{}", fields.join(" "))?)
    }
}

/// Whether directory `ancestor` strictly contains `path` (both normalised).
fn is_ancestor(ancestor: &str, path: &str) -> bool {
    if ancestor == "/" {
        return path != "/";
    }
    path.strip_prefix(ancestor)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Separates leading `-xyz` option arguments from operands. Every option
/// letter must be in `allowed`. A lone `--` ends option parsing.
fn split_flags<'a>(args: &'a [String], allowed: &str) -> Result<(String, &'a [String]), Failure> {
    let mut flags = String::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            index += 1;
            break;
        }
        let Some(letters) = arg.strip_prefix('-').filter(|l| !l.is_empty()) else {
            break;
        };
        for letter in letters.chars() {
            if !allowed.contains(letter) {
                return Err(Failure(format!("invalid option -- '{letter}'")));
            }
            flags.push(letter);
        }
        index += 1;
    }
    Ok((flags, &args[index..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use omit_vfs::ramfs::RamFs;

    const SYSTEM: SystemInfo = SystemInfo {
        sysname: "Omit OS",
        release: "0.1.0",
        machine: "x86_64",
    };

    /// A stand-in for the kernel: records what was started and reports
    /// preset exit statuses.
    #[derive(Default)]
    struct MockHost {
        spawned: Vec<(String, Vec<u8>)>,
        exit_status: i32,
        killed: Vec<u64>,
        pinged: Vec<(omit_net::Ipv4Addr, u16)>,
        last_args: Vec<String>,
        last_cwd: String,
        last_stdout: Option<(String, bool)>,
        last_stdin: Option<String>,
        last_pipeline: Vec<(String, Vec<String>)>,
        formatted: u32,
        now: Option<i64>,
    }

    impl Host for MockHost {
        fn spawn(
            &mut self,
            name: &str,
            image: &[u8],
            args: &[String],
            cwd: &str,
            stdout: Option<(&str, bool)>,
        ) -> Result<u64, String> {
            self.last_stdout = stdout.map(|(path, append)| (path.into(), append));
            if image.starts_with(b"bad") {
                return Err("not an executable".into());
            }
            self.spawned.push((name.into(), image.to_vec()));
            self.last_args = args.to_vec();
            self.last_cwd = cwd.into();
            Ok(self.spawned.len() as u64)
        }

        fn spawn_pipeline(
            &mut self,
            stages: &[Stage<'_>],
            cwd: &str,
            stdin: Option<&str>,
            stdout: Option<(&str, bool)>,
        ) -> Result<Vec<u64>, String> {
            if stages.iter().any(|stage| stage.image.starts_with(b"bad")) {
                return Err("not an executable".into());
            }
            self.last_stdout = stdout.map(|(path, append)| (path.into(), append));
            self.last_stdin = stdin.map(String::from);
            self.last_cwd = cwd.into();
            self.last_pipeline = stages
                .iter()
                .map(|stage| (stage.name.into(), stage.args.to_vec()))
                .collect();
            let mut pids = Vec::new();
            for stage in stages {
                self.spawned.push((stage.name.into(), stage.image.to_vec()));
                pids.push(self.spawned.len() as u64);
            }
            Ok(pids)
        }

        fn kill(&mut self, pid: u64) -> bool {
            self.killed.push(pid);
            pid as usize <= self.spawned.len()
        }

        fn drivers(&mut self) -> Vec<String> {
            alloc::vec!["00:02.0  native     virtio-blk".into()]
        }

        fn network(&mut self) -> Vec<String> {
            alloc::vec!["eth0: test, hardware address 02:00:00:00:00:01".into()]
        }

        fn ping(
            &mut self,
            address: omit_net::Ipv4Addr,
            sequence: u16,
        ) -> Result<PingReply, String> {
            self.pinged.push((address, sequence));
            // The gateway answers; one address loses every second
            // request; nobody else is there.
            match address.0 {
                [10, 0, 2, 2] => Ok(PingReply {
                    micros: 1234,
                    ttl: 255,
                    bytes: 32,
                }),
                [10, 0, 2, 3] if sequence % 2 == 1 => Ok(PingReply {
                    micros: 20_007,
                    ttl: 64,
                    bytes: 32,
                }),
                [10, 0, 2, 3] => Err("no answer within a second".into()),
                _ => Err("unreachable".into()),
            }
        }

        fn device_attribute(
            &mut self,
            device: &str,
            name: Option<&str>,
            value: Option<&str>,
        ) -> Result<Vec<String>, String> {
            if device != "00:04.0" {
                return Err("no Linux driver is bound to that device".into());
            }
            match (name, value) {
                (None, _) => Ok(Vec::from(["events           read/write".into()])),
                (Some("events"), None) => Ok(Vec::from([format!("{:x}", self.formatted)])),
                (Some("events"), Some(value)) => {
                    self.formatted = u32::from_str_radix(value, 16)
                        .map_err(|_| String::from("the driver rejected the value (error 22)"))?;
                    Ok(Vec::new())
                }
                _ => Err("no such attribute".into()),
            }
        }

        fn install(
            &mut self,
            device: &str,
            loader: &[u8],
            kernel: &[u8],
        ) -> Result<Vec<String>, String> {
            if device == "vda" {
                self.formatted += 100;
                Ok(Vec::from([format!(
                    "installed on vda: {} + {} bytes",
                    loader.len(),
                    kernel.len()
                )]))
            } else {
                Err("no such block device".into())
            }
        }

        fn unbind_driver(&mut self, device: &str) -> Result<(), String> {
            if device == "00:03.0" {
                Ok(())
            } else {
                Err("no removable driver is bound to that device".into())
            }
        }

        fn watchdog(&mut self, args: &[String]) -> Result<String, String> {
            match args {
                [] => Ok("test watchdog: stopped\n".into()),
                _ => Err("usage".into()),
            }
        }

        fn pci_devices(&mut self) -> Vec<String> {
            alloc::vec!["00:00.0 8086:29c0 Host bridge".into()]
        }

        fn block_devices(&mut self) -> Vec<(String, u64)> {
            alloc::vec![("vda".into(), 32 << 20)]
        }

        fn format_device(&mut self, device: &str) -> Result<(), String> {
            if device == "vda" {
                self.formatted += 1;
                Ok(())
            } else {
                Err("no such block device".into())
            }
        }

        fn open_filesystem(&mut self, device: &str) -> Result<Box<dyn FileSystem>, String> {
            match (device, self.formatted) {
                ("vda", 0) => Err("not an ext2 filesystem".into()),
                ("vda", _) => Ok(Box::new(RamFs::new())),
                _ => Err("no such block device".into()),
            }
        }

        fn now(&mut self) -> Option<i64> {
            self.now
        }

        fn memory(&mut self) -> MemoryInfo {
            MemoryInfo {
                total_kib: 1000,
                free_kib: 750,
                kernel_heap_kib: 12,
            }
        }

        fn tasks(&mut self) -> Vec<TaskInfo> {
            alloc::vec![
                TaskInfo {
                    thread: 1,
                    pid: None,
                    state: "running",
                    cpu_ms: 120,
                    name: "kernel".into(),
                },
                TaskInfo {
                    thread: 9,
                    pid: Some(3),
                    state: "ready",
                    cpu_ms: 40,
                    name: "spin".into(),
                },
            ]
        }
    }

    struct Session {
        shell: Shell,
        vfs: Vfs,
        host: MockHost,
    }

    impl Session {
        fn new() -> Self {
            Self {
                shell: Shell::new(SYSTEM),
                vfs: Vfs::new(Box::new(RamFs::new())),
                host: MockHost::default(),
            }
        }

        /// Runs `line`, waiting for a foreground program as the kernel
        /// would, and returns everything it printed.
        fn run(&mut self, line: &str) -> String {
            let mut out = String::new();
            match self
                .shell
                .execute(line, &mut self.vfs, &mut self.host, &mut out)
            {
                Outcome::Continue => {}
                Outcome::Wait { pid } => {
                    assert_eq!(pid as usize, self.host.spawned.len());
                    let status = self.host.exit_status;
                    self.shell.finish_wait(Some(status), &mut out);
                }
                Outcome::Exit => panic!("`{line}` exited the shell"),
            }
            out
        }

        /// Runs `line`, asserting that it succeeds.
        fn ok(&mut self, line: &str) -> String {
            let out = self.run(line);
            assert_eq!(self.shell.last_status(), 0, "`{line}` failed: {out}");
            out
        }

        /// Runs `line`, asserting that it fails, and returns the message.
        fn err(&mut self, line: &str) -> String {
            let out = self.run(line);
            assert_ne!(self.shell.last_status(), 0, "`{line}` succeeded: {out}");
            out
        }
    }

    #[test]
    fn echo_pwd_and_prompt() {
        let mut s = Session::new();
        assert_eq!(s.ok("echo hello   world"), "hello world\n");
        assert_eq!(s.ok("echo -n 'a  b'"), "a  b");
        assert_eq!(s.ok("echo"), "\n");
        assert_eq!(s.ok("pwd"), "/\n");
        assert_eq!(s.ok(""), "");
        let mut prompt = String::new();
        s.shell.write_prompt(&mut prompt).unwrap();
        assert_eq!(prompt, "omit:/# ");
    }

    #[test]
    fn the_milestone_3_session() {
        let mut s = Session::new();
        s.ok("mkdir /home");
        s.ok("mkdir /home/user");
        s.ok("cd /home/user");
        assert_eq!(s.ok("pwd"), "/home/user\n");
        s.ok("echo hello from omit > greeting.txt");
        assert_eq!(s.ok("cat greeting.txt"), "hello from omit\n");
        s.ok("echo second line >> greeting.txt");
        assert_eq!(s.ok("cat greeting.txt"), "hello from omit\nsecond line\n");
        s.ok("mkdir docs");
        assert_eq!(s.ok("ls"), "docs\ngreeting.txt\n");
        assert_eq!(s.ok("ls /"), "home\n");
        s.ok("cd ..");
        assert_eq!(s.shell.cwd(), "/home");
        s.ok("cd");
        assert_eq!(s.shell.cwd(), "/");
    }

    #[test]
    fn ls_variants() {
        let mut s = Session::new();
        s.ok("mkdir a b");
        s.ok("echo -n 12345 > a/f");
        assert_eq!(s.ok("ls -l"), "d          1 a\nd          0 b\n");
        assert_eq!(s.ok("ls -l a"), "-          5 f\n");
        assert_eq!(s.ok("ls a/f"), "a/f\n");
        assert_eq!(s.ok("ls a b"), "a:\nf\n\nb:\n");
        assert_eq!(s.err("ls nope"), "ls: nope: No such file or directory\n");
        assert_eq!(s.err("ls -z"), "ls: invalid option -- 'z'\n");
    }

    #[test]
    fn cd_errors() {
        let mut s = Session::new();
        s.ok("touch file");
        assert_eq!(s.err("cd file"), "cd: file: Not a directory\n");
        assert_eq!(s.err("cd nope"), "cd: nope: No such file or directory\n");
        assert_eq!(s.err("cd a b"), "cd: too many arguments\n");
        assert_eq!(s.shell.cwd(), "/");
    }

    #[test]
    fn mkdir_touch_cat() {
        let mut s = Session::new();
        s.ok("mkdir -p x/y/z");
        s.ok("mkdir -p x/y/z");
        assert_eq!(s.err("mkdir x"), "mkdir: x: File exists\n");
        assert_eq!(
            s.err("mkdir q/r"),
            "mkdir: q/r: No such file or directory\n"
        );
        assert_eq!(s.err("mkdir"), "mkdir: missing operand\n");

        s.ok("touch x/t");
        assert_eq!(s.ok("cat x/t"), "");
        s.ok("echo keep > x/t");
        s.ok("touch x/t");
        assert_eq!(s.ok("cat x/t"), "keep\n", "touch must not truncate");
        s.ok("echo a > 1");
        s.ok("echo b > 2");
        assert_eq!(s.ok("cat 1 2"), "a\nb\n");
        assert_eq!(s.err("cat x"), "cat: x: Is a directory\n");
        assert_eq!(s.err("cat"), "cat: missing file operand\n");
    }

    #[test]
    fn cp_and_mv() {
        let mut s = Session::new();
        s.ok("echo data > f");
        s.ok("mkdir d");
        s.ok("cp f g");
        s.ok("cp f d");
        assert_eq!(s.ok("cat g d/f"), "data\ndata\n");
        assert!(s.err("cp d e").contains("Is a directory"));
        assert!(s.err("cp f f").contains("same file"));
        assert_eq!(s.err("cp f"), "cp: usage: cp SOURCE DEST\n");
        assert_eq!(s.err("cp nope x"), "cp: x: No such file or directory\n");

        s.ok("mv g renamed");
        s.ok("mv renamed d");
        assert_eq!(s.ok("ls d"), "f\nrenamed\n");
        s.ok("mv d e");
        assert_eq!(s.ok("ls"), "e\nf\n");
        assert_eq!(s.err("mv nope x"), "mv: nope: No such file or directory\n");
        assert_eq!(s.err("mv e e/inside"), "mv: e/inside: Invalid argument\n");
        assert_eq!(s.err("mv f e/f"), "mv: e/f: File exists\n");
    }

    #[test]
    fn rm_rules() {
        let mut s = Session::new();
        s.ok("mkdir -p d/sub");
        s.ok("touch d/sub/f top");
        assert_eq!(s.err("rm d"), "rm: d: Is a directory\n");
        s.ok("rm top");
        assert_eq!(s.err("rm top"), "rm: top: No such file or directory\n");
        s.ok("cd d/sub");
        assert!(s.err("rm -r /d").contains("refusing"));
        assert!(s.err("rm -r .").contains("refusing"));
        assert!(s.err("rm -r /").contains("refusing"));
        s.ok("cd /");
        s.ok("rm -r d");
        assert_eq!(s.ok("ls"), "");
        assert_eq!(s.err("rm"), "rm: missing operand\n");
    }

    #[test]
    fn redirection_edge_cases() {
        let mut s = Session::new();
        s.ok("mkdir d");
        assert_eq!(s.err("echo x > d"), "echo: d: Is a directory\n");
        assert_eq!(
            s.err("echo x > nope/f"),
            "echo: nope/f: No such file or directory\n"
        );
        // A failing command must not create or clobber the target.
        s.ok("echo keep > f");
        assert_eq!(
            s.err("cat nope > f"),
            "cat: nope: No such file or directory\n"
        );
        assert_eq!(s.ok("cat f"), "keep\n");
        s.ok("ls > listing");
        assert_eq!(s.ok("cat listing"), "d\nf\n");
        assert_eq!(s.err("echo 'x"), "omsh: syntax error: unterminated quote\n");
    }

    #[test]
    fn uname_help_and_unknown_commands() {
        let mut s = Session::new();
        assert_eq!(s.ok("uname"), "Omit OS\n");
        assert_eq!(s.ok("uname -a"), "Omit OS 0.1.0 x86_64\n");
        assert_eq!(s.ok("uname -rm"), "0.1.0 x86_64\n");
        assert!(s.ok("help").contains("mkdir [-p] DIR..."));
        assert_eq!(s.ok("clear"), "\x1b[2J\x1b[H");
        assert_eq!(s.err("frobnicate"), "frobnicate: command not found\n");
    }

    #[test]
    fn runs_programs_from_bin_or_by_path() {
        let mut s = Session::new();
        s.ok("mkdir /bin /opt");
        s.ok("echo -n image-of-hello > /bin/hello");
        s.ok("echo -n other > /opt/tool");

        assert_eq!(s.ok("hello"), "");
        assert_eq!(
            s.host.spawned[0],
            ("hello".into(), b"image-of-hello".to_vec())
        );
        s.ok("cd /opt");
        s.ok("./tool");
        s.ok("/bin/hello");
        assert_eq!(s.host.spawned.len(), 3);
        assert_eq!(s.host.spawned[1].0, "tool");

        // A bare name is only looked up in /bin, never the current directory.
        assert_eq!(s.err("tool"), "tool: command not found\n");
        assert_eq!(s.err("./missing"), "./missing: No such file or directory\n");
        assert_eq!(s.err("/opt"), "/opt: Is a directory\n");

        s.host.exit_status = 7;
        assert_eq!(s.err("hello"), "hello: exited with status 7\n");
        assert_eq!(s.shell.last_status(), 7);
        s.host.exit_status = 0;

        // Arguments and the working directory are passed on.
        s.ok("hello 'two words' -x");
        assert_eq!(s.host.last_args, ["hello", "two words", "-x"]);
        assert_eq!(s.host.last_cwd, "/opt");

        s.ok("echo -n bad-image > /bin/broken");
        assert_eq!(s.err("broken"), "broken: not an executable\n");
    }

    #[test]
    fn pipelines_and_input_redirection() {
        let mut s = Session::new();
        s.ok("mkdir /bin /data");
        for program in ["cat", "tee", "sort"] {
            s.ok(&format!("echo -n image-of-{program} > /bin/{program}"));
        }
        s.ok("echo words > /data/in.txt");
        s.ok("cd /data");

        s.ok("cat < in.txt | tee copy | sort -r > out.txt");
        let stage = |name: &str, args: &[&str]| {
            (
                String::from(name),
                args.iter().map(|&a| String::from(a)).collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            s.host.last_pipeline,
            [
                stage("cat", &["cat"]),
                stage("tee", &["tee", "copy"]),
                stage("sort", &["sort", "-r"]),
            ]
        );
        assert_eq!(s.host.last_stdin.as_deref(), Some("/data/in.txt"));
        assert_eq!(s.host.last_stdout, Some(("/data/out.txt".into(), false)));
        assert_eq!(s.host.last_cwd, "/data");
        // The output file exists (empty) before the programs run.
        assert_eq!(s.ok("cat out.txt"), "");

        // A single program reading from a file; the built-in `cat` gives
        // way to the program of the same name.
        let before = s.host.spawned.len();
        s.ok("cat < in.txt");
        assert_eq!(s.host.spawned.len(), before + 1);
        assert_eq!(s.host.last_pipeline, [stage("cat", &["cat"])]);
        assert_eq!(s.host.last_stdout, None);

        // The exit status is that of the last command.
        s.host.exit_status = 3;
        assert_eq!(s.err("cat | sort"), "sort: exited with status 3\n");
        s.host.exit_status = 0;

        // Background pipelines report the last command.
        let line = s.ok("cat | sort &");
        assert!(line.ends_with("] sort\n"), "{line}");

        // Errors name the command or file they are about, and nothing is
        // started.
        let before = s.host.spawned.len();
        assert_eq!(s.err("cat | nosuch | sort"), "nosuch: command not found\n");
        assert_eq!(
            s.err("cat | pwd"),
            "pwd: this built-in command cannot be used with '|' or '<'\n"
        );
        assert_eq!(
            s.err("sort < missing.txt"),
            "missing.txt: No such file or directory\n"
        );
        assert_eq!(s.err("sort < /data"), "/data: is a directory\n");
        assert_eq!(
            s.err("pwd < in.txt"),
            "pwd: this built-in command cannot be used with '|' or '<'\n"
        );
        assert_eq!(s.host.spawned.len(), before);
        assert_eq!(
            s.err("cat |"),
            "omsh: syntax error: missing command before or after '|'\n"
        );
    }

    #[test]
    fn install_requires_explicit_confirmation() {
        let mut s = Session::new();
        assert_eq!(s.err("install"), "install: usage: install DEVICE --erase\n");
        assert_eq!(
            s.err("install vda"),
            "install: usage: install DEVICE --erase\n"
        );
        assert_eq!(
            s.err("install vda yes"),
            "install: this erases the whole disk; confirm with: install DEVICE --erase\n"
        );
        assert_eq!(s.host.formatted, 0, "nothing was touched");
        // The files the installer copies must exist.
        assert_eq!(
            s.err("install vda --erase"),
            "install: cannot read /boot/BOOTX64.EFI, which the installer copies: \
             No such file or directory\n"
        );
        s.ok("mkdir /boot");
        s.ok("echo -n loader > /boot/BOOTX64.EFI");
        s.ok("echo -n the-kernel > /boot/KERNEL.ELF");
        assert_eq!(
            s.ok("install vda --erase"),
            "installed on vda: 6 + 10 bytes\n"
        );
        assert_eq!(s.host.formatted, 100);
        assert_eq!(
            s.err("install sdz --erase"),
            "install: sdz: no such block device\n"
        );
    }

    #[test]
    fn device_attributes() {
        let mut s = Session::new();
        assert_eq!(s.ok("devattr 00:04.0"), "events           read/write\n");
        assert_eq!(s.ok("devattr 00:04.0 events"), "0\n");
        assert_eq!(s.ok("devattr 00:04.0 events 1f"), "");
        assert_eq!(s.ok("devattr 00:04.0 events"), "1f\n");
        assert_eq!(
            s.err("devattr 00:04.0 events zz"),
            "devattr: 00:04.0: the driver rejected the value (error 22)\n"
        );
        assert_eq!(
            s.err("devattr 00:04.0 nope"),
            "devattr: 00:04.0: no such attribute\n"
        );
        assert_eq!(
            s.err("devattr 00:09.0"),
            "devattr: 00:09.0: no Linux driver is bound to that device\n"
        );
        assert_eq!(
            s.err("devattr"),
            "devattr: usage: devattr DEVICE [NAME [VALUE]]\n"
        );
    }

    #[test]
    fn windows_programs_are_run_by_the_windows_loader() {
        let mut s = Session::new();
        s.ok("mkdir /bin /apps");
        s.ok("echo -n MZ-windows-program > /apps/tool.exe");

        // Without the loader the reason is stated.
        assert_eq!(
            s.err("/apps/tool.exe"),
            "/apps/tool.exe: this is a Windows program, but /bin/winrun cannot be read: \
             No such file or directory\n"
        );

        s.ok("echo -n loader-image > /bin/winrun");
        s.ok("cd /apps");
        s.ok("./tool.exe 'two words' -x");
        // The loader's image runs, under the program's name, with the
        // program's full path inserted before its arguments.
        assert_eq!(
            s.host.spawned[0],
            ("tool.exe".into(), b"loader-image".to_vec())
        );
        assert_eq!(
            s.host.last_args,
            ["/bin/winrun", "/apps/tool.exe", "two words", "-x"]
        );
    }

    #[test]
    fn background_jobs_and_unsupported_program_features() {
        let mut s = Session::new();
        s.ok("mkdir /bin");
        s.ok("echo -n x > /bin/spin");
        s.host.exit_status = 9;
        assert_eq!(
            s.ok("spin &"),
            "[1] spin\n",
            "background jobs are not waited for"
        );
        // Redirection: the target is prepared before the program starts.
        s.host.exit_status = 0;
        s.ok("echo old > out");
        s.ok("spin > out");
        assert_eq!(s.host.last_stdout, Some(("/out".into(), false)));
        assert_eq!(s.ok("cat out"), "", "'>' empties the target first");
        s.ok("echo kept > log");
        s.ok("spin >> log &");
        assert_eq!(s.host.last_stdout, Some(("/log".into(), true)));
        assert_eq!(s.ok("cat log"), "kept\n", "'>>' keeps existing contents");
        s.ok("spin");
        assert_eq!(s.host.last_stdout, None);
        let before = s.host.spawned.len();
        assert_eq!(
            s.err("spin > /nope/out"),
            "spin: /nope/out: No such file or directory\n"
        );
        assert_eq!(
            s.host.spawned.len(),
            before,
            "rejected commands start nothing"
        );
        assert_eq!(
            s.err("pwd &"),
            "pwd: built-in commands cannot run in the background\n"
        );
        assert_eq!(
            s.err("a & b"),
            "omsh: syntax error: '&' is only allowed at the end of a command\n"
        );
    }

    #[test]
    fn devices_and_mounting() {
        let mut s = Session::new();
        assert_eq!(s.ok("lspci"), "00:00.0 8086:29c0 Host bridge\n");
        assert_eq!(s.ok("lsblk"), "NAME        SIZE\nvda       32 MiB\n");
        assert_eq!(
            s.ok("lsdrv"),
            "DEVICE   ORIGIN     DRIVER\n00:02.0  native     virtio-blk\n"
        );
        assert_eq!(s.ok("watchdog"), "test watchdog: stopped\n");
        assert_eq!(s.err("watchdog bogus"), "watchdog: usage\n");
        s.ok("unbind 00:03.0");
        assert_eq!(
            s.err("unbind 00:09.0"),
            "unbind: 00:09.0: no removable driver is bound to that device\n"
        );
        assert_eq!(s.ok("mount"), "ramfs on /\n");

        s.ok("mkdir /mnt");
        assert_eq!(
            s.err("mount vda /mnt"),
            "mount: vda: not an ext2 filesystem\n"
        );
        assert_eq!(s.err("mkfs sdz"), "mkfs: sdz: no such block device\n");
        assert_eq!(
            s.ok("mkfs vda"),
            "mkfs: created an ext2 filesystem on vda\n"
        );
        assert_eq!(
            s.err("mount vda /nope"),
            "mount: /nope: No such file or directory\n"
        );
        s.ok("mount vda /mnt");
        assert_eq!(s.ok("mount"), "ramfs on /\nvda on /mnt\n");
        assert_eq!(
            s.err("mount vda /mnt"),
            "mount: /mnt: Device or resource busy\n"
        );

        s.ok("echo persistent > /mnt/file");
        assert_eq!(s.ok("cat /mnt/file"), "persistent\n");
        assert_eq!(
            s.err("mv /mnt/file /file"),
            "mv: /file: Invalid cross-device link\n"
        );
        s.ok("cp /mnt/file /copy");
        assert_eq!(s.err("rm -r /mnt"), "rm: /mnt: Device or resource busy\n");

        s.ok("cd /mnt");
        assert_eq!(
            s.err("umount /mnt"),
            "umount: /mnt: Device or resource busy\n"
        );
        s.ok("cd /");
        s.ok("umount /mnt");
        assert_eq!(s.ok("mount"), "ramfs on /\n");
        assert_eq!(s.ok("ls /mnt"), "");
        assert_eq!(s.err("umount /mnt"), "umount: /mnt: Invalid argument\n");
        assert_eq!(s.err("umount /"), "umount: /: Device or resource busy\n");
        assert_eq!(s.err("mount vda"), "mount: usage: mount [DEVICE DIR]\n");
    }

    #[test]
    fn ps_and_kill() {
        let mut s = Session::new();
        assert_eq!(
            s.ok("ps"),
            "  PID   TID STATE     CPU(ms) NAME\n    -     1 running       120 kernel\n    3     9 ready          40 spin\n"
        );
        s.ok("mkdir /bin");
        s.ok("echo -n x > /bin/spin");
        s.ok("spin &");
        s.ok("kill 1");
        assert_eq!(s.host.killed, [1]);
        assert_eq!(s.err("kill 42"), "kill: 42: no such process\n");
        assert_eq!(s.err("kill abc"), "kill: abc: not a process ID\n");
        assert_eq!(s.err("kill"), "kill: usage: kill PID\n");
        assert_eq!(s.err("ps -x"), "ps: extra operand\n");
        assert_eq!(
            s.ok("free"),
            "             total        used        free (KiB)\nMemory:       1000         250         750\nKernel heap in use: 12 KiB\n"
        );
    }

    #[test]
    fn exit_is_reported() {
        let mut s = Session::new();
        let mut out = String::new();
        assert_eq!(
            s.shell.execute("exit", &mut s.vfs, &mut s.host, &mut out),
            Outcome::Exit
        );
        assert_eq!(
            s.shell
                .execute("  exit  now", &mut s.vfs, &mut s.host, &mut out),
            Outcome::Exit
        );
    }

    #[test]
    fn cmp_compares_files() {
        let mut s = Session::new();
        s.ok("echo one > /a");
        s.ok("echo one > /b");
        s.ok("echo one more > /c");
        s.ok("echo two > /d");
        assert_eq!(s.ok("cmp /a /b"), "");
        assert_eq!(s.err("cmp /a /d"), "cmp: /a and /d differ at byte 1\n");
        assert_eq!(s.err("cmp /a /c"), "cmp: /a and /c differ at byte 4\n");
        assert!(s.err("cmp /a").contains("usage"));
    }

    #[test]
    fn ifconfig_prints_what_the_system_reports() {
        let mut s = Session::new();
        assert_eq!(
            s.ok("ifconfig"),
            "eth0: test, hardware address 02:00:00:00:00:01\n"
        );
        assert_eq!(s.err("ifconfig eth0"), "ifconfig: usage: ifconfig\n");
    }

    #[test]
    fn ping_reports_each_answer_and_a_summary() {
        let mut s = Session::new();
        assert_eq!(
            s.ok("ping 10.0.2.2"),
            "32 bytes from 10.0.2.2: seq=1 ttl=255 time=1.234 ms\n\
             32 bytes from 10.0.2.2: seq=2 ttl=255 time=1.234 ms\n\
             32 bytes from 10.0.2.2: seq=3 ttl=255 time=1.234 ms\n\
             3 sent, 3 received\n"
        );
        let gateway = omit_net::Ipv4Addr([10, 0, 2, 2]);
        assert_eq!(s.host.pinged, [(gateway, 1), (gateway, 2), (gateway, 3)]);
        // Some lost: still a success, and the losses are shown.
        assert_eq!(
            s.ok("ping 10.0.2.3 4"),
            "32 bytes from 10.0.2.3: seq=1 ttl=64 time=20.007 ms\n\
             seq=2: no answer within a second\n\
             32 bytes from 10.0.2.3: seq=3 ttl=64 time=20.007 ms\n\
             seq=4: no answer within a second\n\
             4 sent, 2 received\n"
        );
        // All lost: a failure.
        assert_eq!(
            s.err("ping 10.0.2.99 1"),
            "seq=1: unreachable\n1 sent, 0 received\nping: no answer from 10.0.2.99\n"
        );
    }

    #[test]
    fn ping_refuses_what_it_cannot_do() {
        let mut s = Session::new();
        assert_eq!(s.err("ping"), "ping: usage: ping ADDRESS [COUNT]\n");
        assert_eq!(
            s.err("ping 1.2.3.4 5 6"),
            "ping: usage: ping ADDRESS [COUNT]\n"
        );
        assert_eq!(
            s.err("ping example.org"),
            "ping: example.org: not an IPv4 address (names cannot be looked up yet)\n"
        );
        for count in ["0", "101", "many", "-1"] {
            assert_eq!(
                s.err(&format!("ping 10.0.2.2 {count}")),
                "ping: COUNT must be between 1 and 100\n"
            );
        }
        assert!(s.host.pinged.is_empty());
    }

    #[test]
    fn date_shows_the_clock_or_says_there_is_none() {
        let mut s = Session::new();
        assert_eq!(s.err("date"), "date: this machine has no usable clock\n");
        s.host.now = Some(1_794_312_000);
        assert_eq!(s.ok("date"), "Tue 2026-11-10 12:00:00 UTC\n");
        assert_eq!(s.err("date tomorrow"), "date: usage: date\n");
    }

    #[test]
    fn ancestor_check() {
        assert!(is_ancestor("/", "/a"));
        assert!(!is_ancestor("/", "/"));
        assert!(is_ancestor("/a", "/a/b"));
        assert!(!is_ancestor("/a", "/ab"));
        assert!(!is_ancestor("/a/b", "/a"));
    }
}
