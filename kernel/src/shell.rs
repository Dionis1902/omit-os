// SPDX-License-Identifier: GPL-2.0-only
//! Runs the interactive shell on the kernel console.
//!
//! The shell itself lives in the `omit_shell` crate; this module provides
//! its environment: console input and output, and a freshly created RAM
//! filesystem as the root.
//!
//! The filesystem is owned by this function rather than being a kernel-wide
//! object. That is deliberate for now: the shell is its only user, and a
//! shared filesystem needs a lock that can be held across blocking
//! operations, which in turn needs the scheduler (Milestone 4).

use alloc::boxed::Box;

use omit_shell::{LineEditor, Outcome, Shell, SystemInfo};
use omit_vfs::Vfs;
use omit_vfs::ramfs::RamFs;

use crate::console::{self, Writer};
use crate::{arch, info, kprintln};

/// Runs the shell until the user types `exit`.
pub fn run() {
    let mut vfs = Vfs::new(Box::new(RamFs::new()));
    let mut shell = Shell::new(SystemInfo {
        sysname: "Omit OS",
        release: env!("CARGO_PKG_VERSION"),
        machine: arch::NAME,
    });
    let mut editor = LineEditor::new();
    let mut out = Writer;

    info!("vfs: RAM filesystem mounted at /");
    kprintln!("Omit OS shell. Type 'help' for a list of commands.");
    loop {
        // Console output cannot fail.
        let _ = shell.write_prompt(&mut out);
        let line = loop {
            if let Some(line) = editor.feed(console::read_byte(), &mut out) {
                break line;
            }
        };
        if shell.execute(&line, &mut vfs, &mut out) == Outcome::Exit {
            break;
        }
    }

    let dropped = console::dropped_input();
    if dropped != 0 {
        crate::warn!("console: {dropped} input byte(s) were dropped");
    }
    info!("shell exited");
}
