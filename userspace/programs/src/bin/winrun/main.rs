// SPDX-License-Identifier: GPL-2.0-only
//! `winrun` — runs a 64-bit Windows console program.
//!
//! ```text
//! winrun PROGRAM.EXE [ARGUMENT...]
//! winrun --list
//! ```
//!
//! This is an ordinary Omit OS program. It reads the executable, lays it
//! out in its own address space the way the PE format prescribes, points
//! the program's imported functions at the implementations in
//! [`kernel32`], and jumps to the entry point. The Windows program then
//! runs directly on the CPU — nothing is emulated or translated — and
//! every Windows API call it makes is an ordinary function call into this
//! process, which carries it out with Omit OS system calls.
//!
//! What it refuses, with a message instead of a crash later:
//!
//! * programs that import a function that is not implemented here
//!   (every missing function is listed);
//! * graphical programs, libraries, .NET programs, 32-bit programs;
//! * programs using thread-local storage or delay-loaded imports;
//! * programs without relocation information, since the image cannot be
//!   placed at an address of its choosing;
//! * sections that are both writable and executable.
//!
//! `--list` prints the implemented functions.

#![no_std]
#![no_main]

extern crate alloc;

mod kernel32;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;

use omit_pe::{Pe, SUBSYSTEM_CONSOLE, SUBSYSTEM_GUI};
use omit_rt::abi::protect;
use omit_rt::{eprintln, println};

omit_rt::entry!(main);

const PAGE: usize = 4096;
/// Exit status when the program cannot be loaded (as shells use for
/// "found but not executable").
const EXIT_CANNOT_RUN: i32 = 126;
/// Exit status when the program needs functions that are missing.
const EXIT_MISSING_IMPORTS: i32 = 127;

/// Stack sizes honoured: the program's request, within these bounds.
const STACK_MIN: u64 = 64 * 1024;
const STACK_MAX: u64 = 8 * 1024 * 1024;

fn main() -> i32 {
    let arguments: Vec<&str> = omit_rt::args().collect();
    match arguments.get(1) {
        None => {
            eprintln!("usage: winrun PROGRAM.EXE [ARGUMENT...]");
            eprintln!("       winrun --list");
            2
        }
        Some(&"--list") => {
            for name in kernel32::NAMES {
                println!("KERNEL32.dll!{name}");
            }
            0
        }
        Some(&path) => match load(path, &arguments[1..]) {
            Ok(program) => run(&program),
            Err((status, message)) => {
                eprintln!("winrun: {path}: {message}");
                status
            }
        },
    }
}

/// A program that is ready to run.
struct Program {
    entry: usize,
    stack_top: usize,
}

type LoadError = (i32, String);

fn cannot_run(message: impl Into<String>) -> LoadError {
    (EXIT_CANNOT_RUN, message.into())
}

fn load(path: &str, arguments: &[&str]) -> Result<Program, LoadError> {
    let file = omit_rt::read_file(path).map_err(|error| cannot_run(format!("{error}")))?;
    let pe = Pe::parse(&file).map_err(|error| cannot_run(format!("{error}")))?;

    if pe.is_dll() {
        return Err(cannot_run("this is a library, not a program"));
    }
    let features = pe.features();
    if features.dotnet {
        return Err(cannot_run(".NET programs are not supported"));
    }
    match pe.subsystem() {
        SUBSYSTEM_CONSOLE => {}
        SUBSYSTEM_GUI => {
            return Err(cannot_run(
                "graphical Windows programs are not supported (there is no window system)",
            ));
        }
        other => {
            return Err(cannot_run(format!(
                "not a console program (subsystem {other})"
            )));
        }
    }
    if features.tls {
        return Err(cannot_run(
            "the program uses thread-local storage, which is not supported",
        ));
    }
    if features.delay_imports {
        return Err(cannot_run(
            "the program uses delay-loaded libraries, which are not supported",
        ));
    }
    if pe.relocations_stripped() {
        return Err(cannot_run(format!(
            "the program has no relocation information and only runs at address {:#x}, \
             which is not available",
            pe.image_base()
        )));
    }
    if let Some(section) = pe.sections().iter().find(|s| s.writable && s.executable) {
        return Err(cannot_run(format!(
            "section {} is both writable and executable",
            section.name()
        )));
    }

    // Resolve every import before touching memory, so that all missing
    // functions can be reported at once.
    let mut bindings = Vec::new();
    let mut missing = Vec::new();
    pe.imports(
        |import| match kernel32::resolve(import.library, import.symbol) {
            Some(address) => bindings.push((import.slot_rva as usize, address)),
            None => missing.push(format!("{}!{}", import.library, import.symbol)),
        },
    )
    .map_err(|error| cannot_run(format!("{error}")))?;
    if !missing.is_empty() {
        for name in &missing {
            eprintln!("winrun: {path}: needs {name}, which is not implemented");
        }
        return Err((
            EXIT_MISSING_IMPORTS,
            format!(
                "{} of the Windows functions it needs are not implemented",
                missing.len()
            ),
        ));
    }

    let size = pe.size_of_image() as usize;
    // Windows places images at multiples of 64 KiB; programs may rely on
    // that.
    let base = omit_rt::map(size, 0x1_0000)
        .map_err(|error| cannot_run(format!("cannot allocate {size} bytes: {error}")))?;
    {
        // SAFETY: `map` returned `size` bytes of zeroed, writable memory
        // that nothing else refers to; the slice is dropped before the
        // memory is used in any other way.
        let image = unsafe { core::slice::from_raw_parts_mut(base, size) };
        pe.build_image(image)
            .and_then(|()| omit_pe::relocate(&pe, image, base as u64))
            .map_err(|error| cannot_run(format!("{error}")))?;
        for &(slot, address) in &bindings {
            // The parser checked that every slot lies inside a section.
            image[slot..slot + 8].copy_from_slice(&(address as u64).to_le_bytes());
        }
    }

    // Access rights: everything read-only, then each section as declared.
    // No memory is ever writable and executable.
    let set = |offset: usize, length: usize, protection: u64| {
        let length = length.next_multiple_of(PAGE);
        // SAFETY: the range lies inside the image mapped above, and no
        // reference into the image is live.
        unsafe { omit_rt::protect(base.wrapping_add(offset), length, protection) }
            .map_err(|error| cannot_run(format!("cannot protect memory: {error}")))
    };
    set(0, size, protect::READ)?;
    for section in pe.sections() {
        let protection = match (section.writable, section.executable) {
            (true, _) => protect::WRITE,
            (false, true) => protect::EXECUTE,
            (false, false) => continue,
        };
        set(
            section.rva as usize,
            section.virtual_size as usize,
            protection,
        )?;
    }

    // The stack: the size the program asks for, within reason, with an
    // unmapped page below it so that an overflow faults instead of
    // overwriting other memory.
    let stack_size = pe.stack_reserve().clamp(STACK_MIN, STACK_MAX) as usize;
    let stack_size = stack_size.next_multiple_of(PAGE);
    let stack = omit_rt::map(stack_size + PAGE, PAGE)
        .map_err(|error| cannot_run(format!("cannot allocate the stack: {error}")))?;
    // SAFETY: the page was just mapped and nothing uses it.
    unsafe { omit_rt::unmap(stack, PAGE) }
        .map_err(|error| cannot_run(format!("cannot set up the stack: {error}")))?;
    let stack_limit = stack as usize + PAGE;
    let stack_top = stack_limit + stack_size;

    kernel32::init(kernel32::Setup {
        image_base: base as usize,
        stack_top,
        stack_limit,
        arguments,
    })
    .map_err(|error| cannot_run(format!("cannot set up the process: {error}")))?;

    Ok(Program {
        entry: base as usize + pe.entry_rva() as usize,
        stack_top,
    })
}

fn run(program: &Program) -> i32 {
    let status: u64;
    // SAFETY: `entry` is the entry point of an image that was validated,
    // mapped, relocated and bound above, inside an executable section.
    // `stack_top` is the 16-byte-aligned end of a fresh stack. The code
    // switches to that stack (keeping the old stack pointer in R12, which
    // the Windows calling convention preserves), reserves the 32 bytes of
    // "shadow space" every Windows function may use above its return
    // address, calls the entry point, and switches back. Registers the
    // Windows convention lets the callee change are declared clobbered.
    // From here on the Windows program is in control of the process; it
    // is confined by the same user-mode protections as any other program.
    unsafe {
        asm!(
            "xchg rsp, r12",
            "sub rsp, 32",
            "call r13",
            "mov rsp, r12",
            inout("r12") program.stack_top => _,
            in("r13") program.entry,
            lateout("rax") status,
            clobber_abi("win64"),
        );
    }
    // Returning from the entry point ends the process with that status,
    // as on Windows.
    status as u32 as i32
}
