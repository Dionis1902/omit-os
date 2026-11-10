// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
// Sources: Microsoft Learn "x64 calling convention" and "x64 ABI conventions";
//   Microsoft "PE Format" specification. Behaviour tests: compat/windows/samples
//   (QEMU tests windows-programs and desktop).
//! `winrun` — runs a 64-bit Windows program.
//!
//! ```text
//! winrun PROGRAM.EXE [ARGUMENT...]
//! winrun --list
//! ```
//!
//! This is an ordinary Omit OS program. It reads the executable and the
//! libraries (DLL files) it uses, lays them out in its own address space
//! the way the PE format prescribes ([`loader`]), points the imported
//! functions at their implementations — functions of `KERNEL32.dll` at
//! the ones in [`kernel32`], functions of `USER32.dll` and `GDI32.dll`
//! at the ones in [`gui`], functions of other libraries at the code of
//! those libraries — and jumps to the entry point. The Windows program
//! then runs directly on the CPU — nothing is emulated or translated —
//! and every Windows API call it makes is an ordinary function call into
//! this process, which carries it out with Omit OS system calls.
//!
//! What it refuses, with a message instead of a crash later:
//!
//! * programs that need a function or library that does not exist here
//!   (every missing item is listed);
//! * .NET programs, 32-bit programs, drivers;
//! * images using thread-local storage or delay-loaded imports;
//! * images without relocation information, since an image cannot be
//!   placed at an address of its choosing;
//! * sections that are both writable and executable.
//!
//! `--list` prints the implemented functions of the system libraries.

#![no_std]
#![no_main]

extern crate alloc;

mod gui;
mod kernel32;
mod loader;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;

use loader::LoadError;
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

/// `DLL_PROCESS_ATTACH`: the reason passed to a library's entry point
/// when it is loaded.
pub const DLL_PROCESS_ATTACH: usize = 1;

fn main() -> i32 {
    let arguments: Vec<&str> = omit_rt::args().collect();
    match arguments.get(1) {
        None => {
            eprintln!("usage: winrun PROGRAM.EXE [ARGUMENT...]");
            eprintln!("       winrun --list");
            2
        }
        Some(&"--list") => {
            for (library, names) in [
                ("GDI32.dll", gui::GDI32_NAMES),
                ("KERNEL32.dll", kernel32::NAMES),
                ("USER32.dll", gui::USER32_NAMES),
            ] {
                for name in names {
                    println!("{library}!{name}");
                }
            }
            0
        }
        Some(&path) => match run(path, &arguments[1..]) {
            Ok(status) => status,
            Err((status, message)) => {
                eprintln!("winrun: {message}");
                status
            }
        },
    }
}

fn cannot_run(message: impl Into<String>) -> (i32, String) {
    (EXIT_CANNOT_RUN, message.into())
}

/// Loads and runs the program; returns its exit status.
fn run(path: &str, arguments: &[&str]) -> Result<i32, (i32, String)> {
    let program = loader::load_program(path).map_err(|error| match error {
        LoadError::Unusable(message) => cannot_run(message),
        LoadError::Missing(items) => {
            for item in &items {
                eprintln!("winrun: {path}: needs {item}, which is not available");
            }
            (
                EXIT_MISSING_IMPORTS,
                format!("{path}: {} of the things it needs are missing", items.len()),
            )
        }
    })?;

    // The stack: the size the program asks for, within reason, with an
    // unmapped page below it so that an overflow faults instead of
    // overwriting other memory.
    let stack_size = program.stack_reserve.clamp(STACK_MIN, STACK_MAX) as usize;
    let stack_size = stack_size.next_multiple_of(PAGE);
    let stack = omit_rt::map(stack_size + PAGE, PAGE)
        .map_err(|error| cannot_run(format!("{path}: cannot allocate the stack: {error}")))?;
    // SAFETY: the page was just mapped and nothing uses it.
    unsafe { omit_rt::unmap(stack, PAGE) }
        .map_err(|error| cannot_run(format!("{path}: cannot set up the stack: {error}")))?;
    let stack_limit = stack as usize + PAGE;
    let stack_top = stack_limit + stack_size;

    kernel32::init(kernel32::Setup {
        image_base: program.base,
        stack_top,
        stack_limit,
        stack_size,
        arguments,
    })
    .map_err(|error| cannot_run(format!("{path}: cannot set up the process: {error}")))?;

    // Libraries first, each after the ones it depends on. The third
    // argument is non-null for libraries loaded with the program.
    for library in &program.initialisers {
        // SAFETY: the entry point of a library that was validated, mapped,
        // relocated and bound by the loader, called as Windows calls it.
        let ok = unsafe {
            call_on_stack(
                library.entry,
                stack_top,
                [library.base, DLL_PROCESS_ATTACH, 1],
            )
        };
        if ok as u32 == 0 {
            return Err(cannot_run(format!(
                "{path}: the library {} failed to initialise",
                library.name
            )));
        }
    }

    // SAFETY: the entry point of the program, prepared by the loader like
    // the libraries above. From here on the Windows program is in control
    // of the process; it is confined by the same user-mode protections as
    // any other program.
    let status = unsafe { call_on_stack(program.entry, stack_top, [0; 3]) };
    // Returning from the entry point ends the process with that status,
    // as on Windows.
    Ok(status as u32 as i32)
}

/// Calls `function` with up to three integer arguments by the Windows x64
/// calling convention, on the stack that ends at `stack_top`, and returns
/// what it returns.
///
/// # Safety
///
/// `function` must be the address of code that follows that convention
/// and that is safe to run with these arguments; `stack_top` must be the
/// 16-byte-aligned end of a stack nothing else is using.
pub(crate) unsafe fn call_on_stack(
    function: usize,
    stack_top: usize,
    arguments: [usize; 3],
) -> u64 {
    let result: u64;
    // SAFETY: the code switches to the given stack (keeping the old stack
    // pointer in R12, which the Windows convention preserves), reserves
    // the 32 bytes of "shadow space" every Windows function may use above
    // its return address, calls the function with its arguments in RCX,
    // RDX and R8, and switches back. Registers the convention lets the
    // callee change are declared clobbered. The rest is the caller's
    // contract.
    unsafe {
        asm!(
            "xchg rsp, r12",
            "sub rsp, 32",
            "call r13",
            "mov rsp, r12",
            inout("r12") stack_top => _,
            in("r13") function,
            inout("rcx") arguments[0] => _,
            inout("rdx") arguments[1] => _,
            inout("r8") arguments[2] => _,
            lateout("rax") result,
            clobber_abi("win64"),
        );
    }
    result
}
