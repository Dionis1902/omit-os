// SPDX-License-Identifier: GPL-2.0-only
//! Runs the interactive shell on the kernel console.
//!
//! The shell itself lives in the `omit_shell` crate; this module provides
//! its environment: console input and output, a RAM filesystem populated
//! from the initial ramdisk, and the ability to run programs as processes.
//!
//! The filesystem is owned by this function rather than being a kernel-wide
//! object. The shell is still its only user: programs have no file system
//! calls yet.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use omit_shell::{Host, LineEditor, MemoryInfo, Outcome, Shell, SystemInfo, TaskInfo};
use omit_vfs::ramfs::RamFs;
use omit_vfs::{Vfs, tar};

use crate::console::{self, Writer};
use crate::{arch, info, kprintln, mm, process, sched, time, warn};

/// Connects the shell's process commands to the kernel.
struct KernelHost;

impl Host for KernelHost {
    fn spawn(&mut self, name: &str, image: &[u8]) -> Result<u64, String> {
        process::spawn(name, image).map_err(|error| format!("{error}"))
    }

    fn wait(&mut self, pid: u64) -> Option<i32> {
        process::wait(pid)
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

/// Runs the shell until the user types `exit`. `initrd` is the initial
/// ramdisk (a tar archive, possibly empty) to unpack into the root
/// filesystem.
pub fn run(initrd: &[u8]) {
    let mut vfs = Vfs::new(Box::new(RamFs::new()));
    info!("vfs: RAM filesystem mounted at /");
    if !initrd.is_empty() {
        match tar::unpack(&mut vfs, initrd) {
            Ok(files) => info!(
                "vfs: unpacked {files} file(s) from the initial ramdisk ({} KiB)",
                initrd.len() / 1024
            ),
            Err(error) => warn!("vfs: initial ramdisk is damaged: {error}"),
        }
    }

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
        if shell.execute(&line, &mut vfs, &mut host, &mut out) == Outcome::Exit {
            break;
        }
    }

    let dropped = console::dropped_input();
    if dropped != 0 {
        warn!("console: {dropped} input byte(s) were dropped");
    }
    info!("shell exited");
}
