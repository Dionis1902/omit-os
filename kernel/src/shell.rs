// SPDX-License-Identifier: GPL-2.0-only
//! Runs the interactive shell on the kernel console.
//!
//! The shell itself lives in the `omit_shell` crate; this module provides
//! its environment: console input and output, the root filesystem, and the
//! ability to run programs as processes.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use omit_shell::{Host, LineEditor, MemoryInfo, Outcome, Shell, SystemInfo, TaskInfo};
use omit_vfs::FileSystem;

use crate::console::{self, Writer};
use crate::process::OpenFile;
use crate::{arch, fs, info, kprintln, mm, process, sched, time, warn};

/// Connects the shell's process commands to the kernel.
struct KernelHost;

impl Host for KernelHost {
    fn spawn(
        &mut self,
        name: &str,
        image: &[u8],
        args: &[String],
        cwd: &str,
        stdout: Option<(&str, bool)>,
    ) -> Result<u64, String> {
        let stdout = stdout.map(|(path, append)| OpenFile {
            path: path.into(),
            offset: 0,
            writable: true,
            append,
        });
        process::spawn(name, image, args, cwd, stdout).map_err(|error| format!("{error}"))
    }

    fn pci_devices(&mut self) -> Vec<String> {
        crate::pci::devices()
            .iter()
            .map(|device| format!("{device}"))
            .collect()
    }

    fn block_devices(&mut self) -> Vec<(String, u64)> {
        crate::drivers::block::list()
            .into_iter()
            .map(|(name, sectors)| (name, sectors * crate::drivers::block::SECTOR_SIZE as u64))
            .collect()
    }

    fn format_device(&mut self, device: &str) -> Result<(), String> {
        let mut disk = fs::Disk::open(device).map_err(String::from)?;
        omit_ext2::format(&mut disk).map_err(|error| format!("{error}"))
    }

    fn open_filesystem(&mut self, device: &str) -> Result<Box<dyn FileSystem>, String> {
        let disk = fs::Disk::open(device).map_err(String::from)?;
        match omit_ext2::Ext2::mount(disk) {
            Ok(filesystem) => Ok(Box::new(filesystem)),
            Err(error) => Err(format!("{error}")),
        }
    }

    fn kill(&mut self, pid: u64) -> bool {
        process::kill(pid)
    }

    fn memory(&mut self) -> MemoryInfo {
        let (total, free) = mm::frame::stats().map_or((0, 0), |stats| {
            let kib = |frames: usize| frames as u64 * mm::PAGE_SIZE / 1024;
            (kib(stats.managed), kib(stats.free))
        });
        MemoryInfo {
            total_kib: total,
            free_kib: free,
            kernel_heap_kib: (mm::heap::stats().allocated_bytes / 1024) as u64,
        }
    }

    fn tasks(&mut self) -> Vec<TaskInfo> {
        let tick_ms = 1000 / u64::from(time::TICK_HZ);
        sched::threads()
            .into_iter()
            .map(|thread| TaskInfo {
                thread: thread.id,
                pid: thread.pid,
                state: thread.state.name(),
                cpu_ms: thread.cpu_ticks * tick_ms,
                name: thread.name,
            })
            .collect()
    }
}

/// Runs the shell until the user types `exit`. The root filesystem must
/// have been initialised.
pub fn run() {
    let mut shell = Shell::new(SystemInfo {
        sysname: "Omit OS",
        release: env!("CARGO_PKG_VERSION"),
        machine: arch::NAME,
    });
    let mut host = KernelHost;
    let mut editor = LineEditor::new();
    let mut out = Writer;

    kprintln!("Omit OS shell. Type 'help' for a list of commands.");
    loop {
        // Background jobs that have finished leave nothing behind.
        process::reap_finished();
        // Console output cannot fail.
        let _ = shell.write_prompt(&mut out);
        let line = loop {
            if let Some(line) = editor.feed(console::read_byte(), &mut out) {
                break line;
            }
        };

        // The filesystem is locked only while the shell itself works on
        // it. It must be free again before waiting for a program, which
        // may use it.
        let outcome = match fs::root() {
            Some(mut root) => shell.execute(&line, &mut root, &mut host, &mut out),
            None => {
                warn!("shell: no root filesystem");
                break;
            }
        };
        match outcome {
            Outcome::Continue => {}
            Outcome::Exit => break,
            Outcome::Wait { pid } => shell.finish_wait(process::wait(pid), &mut out),
        }
    }

    let dropped = console::dropped_input();
    if dropped != 0 {
        warn!("console: {dropped} input byte(s) were dropped");
    }
    info!("shell exited");
}
