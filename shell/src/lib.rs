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

    /// Requests termination of process `pid`; `false` if it does not exist.
    fn kill(&mut self, pid: u64) -> bool;

    /// Lists all threads and processes.
    fn tasks(&mut self) -> Vec<TaskInfo>;

    /// Current memory usage.
    fn memory(&mut self) -> MemoryInfo;

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

    /// Runs a watchdog subcommand and returns what to print.
    fn watchdog(&mut self, args: &[String]) -> Result<String, String>;

    /// Opens the filesystem on block device `device` for mounting.
    fn open_filesystem(&mut self, device: &str) -> Result<Box<dyn FileSystem>, String>;
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
    "watchdog",
];

/// Internal marker returned by `run` for names that are not built-ins.
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
  mv SOURCE DEST       move or rename a file or directory
  rm [-r] PATH...      remove files (-r: and directories, recursively)
  ps                   list threads and processes
  kill PID             terminate a process
  free                 show memory usage
  lspci                list PCI devices
  lsblk                list block devices
  lsdrv                list drivers and the devices they control
  unbind DEVICE        detach the driver from a device (e.g. 00:03.0)
  watchdog [COMMAND]   status, start, stop, ping, timeout SECONDS
  mkfs DEVICE          create an empty ext2 filesystem (erases the device)
  mount [DEVICE DIR]   mount a filesystem, or list what is mounted
  umount DIR           unmount a filesystem
  uname [-a]           print system information
  clear                clear the screen
  exit                 leave the shell
Anything else is run as a program from /bin (or by path); end the line with
'&' to run it in the background. Output of any command can be redirected
with '> FILE' or '>> FILE'.
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
            "watchdog" => Ok(out.write_str(&host.watchdog(args).map_err(Failure)?)?),
            "lsblk" => {
                writeln!(out, "NAME        SIZE")?;
                for (device, bytes) in host.block_devices() {
                    writeln!(out, "{device:<6} {:>5} MiB", bytes >> 20)?;
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

    /// Runs `name` as a program: `/bin/<name>`, or the path itself if it
    /// contains a slash.
    #[allow(clippy::too_many_arguments)]
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
        // Prepare the redirection target: it must exist, and `>` empties
        // it, before the program starts writing.
        let target = match redirect {
            Some(redirect) => {
                let target = self.resolve(&redirect.path)?;
                let mode = if redirect.append {
                    WriteMode::Append
                } else {
                    WriteMode::Truncate
                };
                vfs.write_file(&target, b"", mode)
                    .map_err(|e| fs_failure(&redirect.path, e))?;
                Some((target, redirect.append))
            }
            None => None,
        };

        let display_name = path::file_name(&path).unwrap_or(name);
        // By convention the first argument is the name the program was
        // invoked by.
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(String::from(name));
        argv.extend_from_slice(args);
        let stdout = target
            .as_ref()
            .map(|(path, append)| (path.as_str(), *append));
        let pid = host
            .spawn(display_name, &image, &argv, &self.cwd, stdout)
            .map_err(Failure)?;
        if background {
            writeln!(out, "[{pid}] {display_name}")?;
        } else {
            self.foreground = Some(display_name.into());
            self.pending_wait = Some(pid);
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
        last_args: Vec<String>,
        last_cwd: String,
        last_stdout: Option<(String, bool)>,
        formatted: u32,
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

        fn kill(&mut self, pid: u64) -> bool {
            self.killed.push(pid);
            pid as usize <= self.spawned.len()
        }

        fn drivers(&mut self) -> Vec<String> {
            alloc::vec!["00:02.0  native     virtio-blk".into()]
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
    fn ancestor_check() {
        assert!(is_ancestor("/", "/a"));
        assert!(!is_ancestor("/", "/"));
        assert!(is_ancestor("/a", "/a/b"));
        assert!(!is_ancestor("/a", "/ab"));
        assert!(!is_ancestor("/a/b", "/a"));
    }
}
