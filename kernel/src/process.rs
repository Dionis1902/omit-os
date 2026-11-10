// SPDX-License-Identifier: GPL-2.0-only
//! User processes.
//!
//! A process is a user address space plus one kernel thread that runs it.
//! Creating one means building the address space from an ELF image and
//! starting a thread that drops to user mode at the program's entry point.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use core::fmt;

use omit_abi::{EXIT_KILLED, USER_STACK_SIZE, USER_STACK_TOP};
use omit_elf::{EM_X86_64, ET_EXEC, Elf64, ElfError};

use crate::arch::gdt::{USER_CS, USER_DS};
use crate::arch::paging::MapError;
use crate::arch::syscall::enter_user;
use crate::arch::uspace::{self, AddressSpace};
use crate::mm::{self, PAGE_SIZE, frame};
use crate::sched::{self, SpawnError, ThreadId};
use crate::sync::SpinLock;

/// Identifies a process. Never reused.
pub type Pid = u64;

/// Why a program could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecError {
    /// The file is not a valid ELF image.
    Elf(ElfError),
    /// The image is valid ELF but not a program this kernel can run.
    Unsupported(&'static str),
    /// Building the address space failed.
    Map(MapError),
    /// The thread could not be created.
    Spawn(SpawnError),
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elf(e) => write!(f, "not an executable: {e}"),
            Self::Unsupported(what) => write!(f, "cannot execute: {what}"),
            Self::Map(e) => write!(f, "cannot build address space: {e}"),
            Self::Spawn(e) => write!(f, "cannot create thread: {e:?}"),
        }
    }
}

struct Record {
    thread: ThreadId,
    /// Set when the process ends.
    exit_status: Option<i32>,
}

struct Table {
    processes: BTreeMap<Pid, Record>,
    next_pid: Pid,
}

static TABLE: SpinLock<Table> = SpinLock::new(Table {
    processes: BTreeMap::new(),
    next_pid: 1,
});

/// Where a new process starts executing.
struct Start {
    entry: u64,
    stack: u64,
}

/// Allocates a zeroed frame, fills it with `data` at `offset`, and maps it
/// at `virt`.
fn map_page(
    space: &mut AddressSpace,
    virt: u64,
    data: &[u8],
    offset: usize,
    writable: bool,
    executable: bool,
) -> Result<(), ExecError> {
    let phys = frame::alloc_zeroed(0).ok_or(ExecError::Map(MapError::OutOfMemory))?;
    if !data.is_empty() {
        let page = mm::phys_to_virt(phys, PAGE_SIZE).expect("frames are in the direct map");
        // SAFETY: the frame was just allocated and is referenced by nothing
        // else; `offset + data.len() <= PAGE_SIZE` is ensured by the caller.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), page.add(offset), data.len()) };
    }
    space.map(virt, phys, writable, executable).map_err(|e| {
        // SAFETY: the mapping failed, so the frame is still only ours.
        let _ = unsafe { frame::free(phys, 0) };
        ExecError::Map(e)
    })
}

/// Builds a user address space from an ELF image.
fn load(image: &[u8]) -> Result<(AddressSpace, Start), ExecError> {
    let elf = Elf64::parse(image).map_err(ExecError::Elf)?;
    if elf.machine() != EM_X86_64 {
        return Err(ExecError::Unsupported("not an x86-64 program"));
    }
    if elf.elf_type() != ET_EXEC {
        return Err(ExecError::Unsupported(
            "only statically linked, fixed-address programs are supported",
        ));
    }

    let mut space = AddressSpace::new().map_err(ExecError::Map)?;
    let mut entry_is_code = false;
    for segment in elf.load_segments().filter(|s| s.memsz != 0) {
        if !segment.vaddr.is_multiple_of(PAGE_SIZE) {
            return Err(ExecError::Unsupported("segment is not page-aligned"));
        }
        if !uspace::is_user_range(segment.vaddr, segment.memsz) {
            return Err(ExecError::Unsupported("segment outside user address space"));
        }
        if segment.is_writable() && segment.is_executable() {
            return Err(ExecError::Unsupported("segment is writable and executable"));
        }
        if segment.is_executable() && (segment.vaddr..segment.vaddr_end()).contains(&elf.entry()) {
            entry_is_code = true;
        }

        let data = elf.segment_data(&segment);
        for page in 0..segment.memsz.div_ceil(PAGE_SIZE) {
            let start = (page * PAGE_SIZE) as usize;
            let chunk = data
                .get(start..)
                .map_or(&[][..], |rest| &rest[..rest.len().min(PAGE_SIZE as usize)]);
            map_page(
                &mut space,
                segment.vaddr + page * PAGE_SIZE,
                chunk,
                0,
                segment.is_writable(),
                segment.is_executable(),
            )?;
        }
    }
    if !entry_is_code {
        return Err(ExecError::Unsupported(
            "entry point is not in executable code",
        ));
    }

    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    for page in 0..USER_STACK_SIZE / PAGE_SIZE {
        map_page(
            &mut space,
            stack_bottom + page * PAGE_SIZE,
            &[],
            0,
            true,
            false,
        )?;
    }
    Ok((
        space,
        Start {
            entry: elf.entry(),
            stack: USER_STACK_TOP,
        },
    ))
}

/// Kernel-side entry of a process thread: drops to user mode.
fn enter(start: usize) {
    // SAFETY: `start` is the pointer produced by `Box::into_raw` in
    // `spawn`, passed to exactly one thread.
    let start = unsafe { Box::from_raw(start as *mut Start) };
    let (entry, stack) = (start.entry, start.stack);
    drop(start);
    // SAFETY: this thread was created by `spawn_process` with the address
    // space built by `load`, which is active now: it maps `entry` inside an
    // executable user segment and the stack below `stack`. The scheduler
    // registered this thread's kernel stack when it switched to it.
    unsafe { enter_user(entry, stack, u64::from(USER_CS), u64::from(USER_DS)) }
}

/// Starts the program in `image` as a new process called `name`.
pub fn spawn(name: &str, image: &[u8]) -> Result<Pid, ExecError> {
    let (space, start) = load(image)?;
    let start = Box::into_raw(Box::new(start));

    let mut table = TABLE.lock();
    let pid = table.next_pid;
    match sched::spawn_process(name, enter, start as usize, space, pid) {
        Ok(thread) => {
            table.next_pid += 1;
            table.processes.insert(
                pid,
                Record {
                    thread,
                    exit_status: None,
                },
            );
            Ok(pid)
        }
        Err(error) => {
            // SAFETY: no thread was created, so the pointer was never
            // handed over and is still uniquely owned here.
            drop(unsafe { Box::from_raw(start) });
            Err(ExecError::Spawn(error))
        }
    }
}

/// Ends the calling process with `status`. Must be called from a process's
/// own thread.
pub fn exit(status: i32) -> ! {
    if let Some(pid) = sched::current_pid()
        && let Some(record) = TABLE.lock().processes.get_mut(&pid)
    {
        record.exit_status = Some(status);
    }
    sched::exit()
}

/// Ends the calling process if it has been killed. Called on the way back
/// to user mode.
pub fn exit_if_killed() {
    if sched::current_killed() {
        exit(EXIT_KILLED);
    }
}

/// Waits for process `pid` to end and returns its exit status, or `None`
/// if there is no such process (or it was already waited for).
pub fn wait(pid: Pid) -> Option<i32> {
    let thread = TABLE.lock().processes.get(&pid)?.thread;
    sched::join(thread);
    let record = TABLE.lock().processes.remove(&pid)?;
    // A thread can only die without a status if the kernel ended it.
    Some(record.exit_status.unwrap_or(EXIT_KILLED))
}

/// Discards the records of finished processes that nobody waited for, such
/// as background jobs.
pub fn reap_finished() {
    TABLE
        .lock()
        .processes
        .retain(|_, record| sched::is_alive(record.thread));
}

/// Requests termination of process `pid`.
pub fn kill(pid: Pid) -> bool {
    sched::kill(pid)
}

/// Describes why the current process is being terminated after a CPU
/// exception, for the console.
pub fn describe_current() -> (Pid, String) {
    let pid = sched::current_pid().unwrap_or(0);
    let name = sched::threads()
        .into_iter()
        .find(|thread| thread.pid == Some(pid))
        .map(|thread| thread.name)
        .unwrap_or_default();
    (pid, name)
}
