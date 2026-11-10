// SPDX-FileCopyrightText: 2026 Omit OS contributors
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

use omit_shell::{Host, LineEditor, MemoryInfo, Outcome, Shell, Stage, SystemInfo, TaskInfo};
use omit_vfs::FileSystem;

use crate::console::{self, Writer};
use crate::process::{Descriptor, OpenFile};
use crate::{arch, fs, info, kprintln, mm, pipe, process, sched, time, warn};

/// Connects the shell's process commands to the kernel.
struct KernelHost;

/// The descriptor for a program's redirected output. Every write goes to
/// the end of the file, whether the redirection was `>` or `>>` (the
/// shell has already emptied the file for `>`): the program may start
/// others, which inherit the descriptor, and positions kept separately
/// by each of them would make them overwrite one another's output.
fn output_file((path, _append): (&str, bool)) -> Descriptor {
    Descriptor::File(OpenFile {
        path: path.into(),
        offset: 0,
        readable: false,
        writable: true,
        append: true,
    })
}

impl Host for KernelHost {
    fn spawn(
        &mut self,
        name: &str,
        image: &[u8],
        args: &[String],
        cwd: &str,
        stdout: Option<(&str, bool)>,
    ) -> Result<u64, String> {
        let stdout = stdout.map(output_file);
        process::spawn(name, image, args, cwd, [None, stdout, None])
            .map_err(|error| format!("{error}"))
    }

    fn spawn_pipeline(
        &mut self,
        stages: &[Stage<'_>],
        cwd: &str,
        stdin: Option<&str>,
        stdout: Option<(&str, bool)>,
    ) -> Result<Vec<u64>, String> {
        let mut pids = Vec::with_capacity(stages.len());
        // What the next program reads from: the input file for the first,
        // then the reading end of the pipe from the program before.
        let mut input = stdin.map(|path| {
            Descriptor::File(OpenFile {
                path: path.into(),
                offset: 0,
                readable: true,
                writable: false,
                append: false,
            })
        });
        for (index, stage) in stages.iter().enumerate() {
            let last = index + 1 == stages.len();
            let (output, next_input) = if last {
                let file = stdout.map(output_file);
                (file, None)
            } else {
                let (reader, writer) = pipe::new();
                (
                    Some(Descriptor::PipeWrite(writer)),
                    Some(Descriptor::PipeRead(reader)),
                )
            };
            // The descriptors move into the new process; the shell keeps
            // no copy, so each program sees the end of its input when the
            // one before it is done.
            let stdio = [input.take(), output, None];
            match process::spawn(stage.name, stage.image, stage.args, cwd, stdio) {
                Ok(pid) => pids.push(pid),
                Err(error) => {
                    // Stop what was already started. Their pipe ends are
                    // gone with them, and they are reaped like background
                    // jobs.
                    for &pid in &pids {
                        sched::kill(pid);
                    }
                    return Err(format!("{}: {error}", stage.name));
                }
            }
            input = next_input;
        }
        Ok(pids)
    }

    fn pci_devices(&mut self) -> Vec<String> {
        crate::pci::devices()
            .iter()
            .map(|device| {
                // Name and support status from the hardware database; class
                // name for devices it does not know.
                match omit_hwdb::lookup(device.vendor_id, device.device_id) {
                    Some(known) => {
                        let (status, driver) = omit_hwdb::describe(known.support);
                        let driver = if driver.is_empty() {
                            String::new()
                        } else {
                            format!(": {driver}")
                        };
                        format!(
                            "{:02x}:{:02x}.{} {:04x}:{:04x} {} [{status}{driver}]",
                            device.bus,
                            device.device,
                            device.function,
                            device.vendor_id,
                            device.device_id,
                            known.name
                        )
                    }
                    None => format!("{device} [unknown device, no driver]"),
                }
            })
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

    fn drivers(&mut self) -> Vec<String> {
        crate::drivers::registry::bindings()
            .iter()
            .map(|binding| {
                format!(
                    "{:<8} {:<10} {}",
                    binding.device,
                    binding.origin.label(),
                    binding.driver
                )
            })
            .collect()
    }

    fn device_attribute(
        &mut self,
        device: &str,
        name: Option<&str>,
        value: Option<&str>,
    ) -> Result<Vec<String>, String> {
        use crate::linuxkpi;
        match (name, value) {
            (None, _) => {
                let attributes = linuxkpi::attributes(device)
                    .ok_or("no Linux driver is bound to that device")?;
                Ok(attributes
                    .iter()
                    .map(|attribute| {
                        let access = if attribute.writable {
                            "read/write"
                        } else {
                            "read-only"
                        };
                        format!("{:<16} {access}", attribute.name)
                    })
                    .collect())
            }
            (Some(name), None) => {
                let text = linuxkpi::read_attribute(device, name)?;
                Ok(text.lines().map(String::from).collect())
            }
            (Some(name), Some(value)) => {
                linuxkpi::write_attribute(device, name, value)?;
                Ok(Vec::new())
            }
        }
    }

    fn install(
        &mut self,
        device: &str,
        loader: &[u8],
        kernel: &[u8],
    ) -> Result<Vec<String>, String> {
        crate::install::install(device, loader, kernel)
    }

    fn unbind_driver(&mut self, device: &str) -> Result<(), String> {
        if crate::linuxkpi::unbind(device) {
            Ok(())
        } else {
            Err("no removable driver is bound to that device".into())
        }
    }

    fn watchdog(&mut self, args: &[String]) -> Result<String, String> {
        use crate::linuxkpi::watchdog;
        let describe = || {
            watchdog::status()
                .map(|status| {
                    format!(
                        "{}: {}, timeout {} s (range {}-{} s){}\n",
                        status.identity,
                        if status.active { "running" } else { "stopped" },
                        status.timeout,
                        status.limits.0,
                        status.limits.1,
                        if status.caused_last_reset {
                            ", caused the last reset"
                        } else {
                            ""
                        }
                    )
                })
                .ok_or_else(|| String::from("no watchdog device"))
        };
        let done = |result: Result<(), &'static str>, message: &str| {
            result
                .map(|()| format!("watchdog: {message}\n"))
                .map_err(String::from)
        };
        match args {
            [] => describe(),
            [command] if command == "status" => describe(),
            [command] if command == "start" => done(watchdog::start(), "started"),
            [command] if command == "stop" => done(watchdog::stop(), "stopped"),
            [command] if command == "ping" => done(watchdog::ping(), "keep-alive sent"),
            [command, seconds] if command == "timeout" => {
                let seconds: u32 = seconds
                    .parse()
                    .map_err(|_| format!("{seconds}: not a number of seconds"))?;
                done(
                    watchdog::set_timeout(seconds),
                    &format!("timeout set to {seconds} s"),
                )
            }
            _ => Err("usage: watchdog [status|start|stop|ping|timeout SECONDS]".into()),
        }
    }

    fn kill(&mut self, pid: u64) -> bool {
        process::kill(pid)
    }

    fn network(&mut self) -> Vec<String> {
        crate::net::describe()
    }

    fn ping(
        &mut self,
        address: omit_net::Ipv4Addr,
        sequence: u16,
    ) -> Result<omit_shell::PingReply, String> {
        crate::net::ping(address, sequence)
            .map(|reply| omit_shell::PingReply {
                micros: reply.micros,
                ttl: reply.ttl,
                bytes: reply.bytes,
            })
            .map_err(|error| format!("{error}"))
    }

    fn now(&mut self) -> Option<i64> {
        crate::time::realtime_us().map(|micros| (micros / 1_000_000) as i64)
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

    // An installed system has a data partition; make it available. This
    // goes through the shell's own commands so that `mount` and `umount`
    // know about it like any other mount.
    if let Some(partition) = crate::drivers::block::data_partition()
        && let Some(mut root) = fs::root()
    {
        let mut report = String::new();
        shell.execute("mkdir /data", &mut root, &mut host, &mut report);
        shell.execute(
            &format!("mount {partition} /data"),
            &mut root,
            &mut host,
            &mut report,
        );
        drop(root);
        if report.is_empty() {
            info!("fs: data partition {partition} mounted at /data");
        } else {
            warn!(
                "fs: cannot mount data partition {partition}: {}",
                report.trim()
            );
        }
    }

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
    // The system is going down in an orderly way: give drivers that asked
    // for it the chance to act (Linux "sys-off" handlers).
    let handlers = crate::linuxkpi::notify::call_power_off_handlers();
    if handlers != 0 {
        info!("linuxkpi: {handlers} power-off handler(s) called");
    }
}
