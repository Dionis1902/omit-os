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
use core::fmt::{self, Write};

use omit_vfs::{FileType, FsError, Vfs, WriteMode, path};

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

/// What the caller should do after a command has run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Read the next command.
    Continue,
    /// The user asked to leave the shell.
    Exit,
}

/// A command failure: the message is printed as `command: message`.
struct Failure(String);

impl From<fmt::Error> for Failure {
    fn from(_: fmt::Error) -> Self {
        Self("output error".into())
    }
}

type CommandResult = Result<(), Failure>;

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
  uname [-a]           print system information
  clear                clear the screen
  exit                 leave the shell
Output of any command can be redirected with '> FILE' or '>> FILE'.
Not available yet: ps, kill (there is no process management).
";

/// Shell state: the current directory and the last exit status.
#[derive(Debug)]
pub struct Shell {
    cwd: String,
    system: SystemInfo,
    last_status: i32,
}

impl Shell {
    /// Creates a shell whose current directory is `/`.
    pub fn new(system: SystemInfo) -> Self {
        Self {
            cwd: "/".into(),
            system,
            last_status: 0,
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
    pub fn execute(&mut self, line: &str, vfs: &mut Vfs, out: &mut dyn Write) -> Outcome {
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
        let mut captured = String::new();
        let result = {
            let stdout: &mut dyn Write = if command.redirect.is_some() {
                &mut captured
            } else {
                &mut *out
            };
            self.run(name, args, vfs, stdout)
        };
        let result = result.and_then(|()| match &command.redirect {
            Some(redirect) => {
                let target = self.resolve(&redirect.path)?;
                let mode = if redirect.append {
                    WriteMode::Append
                } else {
                    WriteMode::Truncate
                };
                vfs.write_file(&target, captured.as_bytes(), mode)
                    .map_err(|e| fs_failure(&redirect.path, e))
            }
            None => Ok(()),
        });

        self.last_status = match result {
            Ok(()) => 0,
            Err(Failure(message)) => {
                let _ = writeln!(out, "{name}: {message}");
                1
            }
        };
        Outcome::Continue
    }

    fn run(
        &mut self,
        name: &str,
        args: &[String],
        vfs: &mut Vfs,
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
            "ps" | "kill" => Err(Failure(
                "not available: Omit OS has no process management yet".into(),
            )),
            _ => Err(Failure("command not found".into())),
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

    struct Session {
        shell: Shell,
        vfs: Vfs,
    }

    impl Session {
        fn new() -> Self {
            Self {
                shell: Shell::new(SYSTEM),
                vfs: Vfs::new(Box::new(RamFs::new())),
            }
        }

        /// Runs `line` and returns everything it printed.
        fn run(&mut self, line: &str) -> String {
            let mut out = String::new();
            assert_eq!(
                self.shell.execute(line, &mut self.vfs, &mut out),
                Outcome::Continue
            );
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
        assert!(s.err("ps").contains("no process management"));
        assert!(s.err("kill 1").contains("no process management"));
    }

    #[test]
    fn exit_is_reported() {
        let mut s = Session::new();
        let mut out = String::new();
        assert_eq!(s.shell.execute("exit", &mut s.vfs, &mut out), Outcome::Exit);
        assert_eq!(
            s.shell.execute("  exit  now", &mut s.vfs, &mut out),
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
