// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! User processes.
//!
//! A process is a user address space plus one kernel thread that runs it,
//! a working directory and a table of open files. Creating one means
//! building the address space from an ELF image and starting a thread that
//! drops to user mode at the program's entry point.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use omit_abi::{
    ARGS_MAX, EXIT_KILLED, MAP_MAX, MAX_OPEN_FILES, USER_MAP_BASE, USER_MAP_END, USER_STACK_SIZE,
    USER_STACK_TOP,
};
use omit_elf::{EM_X86_64, ET_EXEC, Elf64, ElfError};

use crate::arch::gdt::{USER_CS, USER_DS};
use crate::arch::paging::MapError;
use crate::arch::syscall::enter_user;
use crate::arch::uspace::{self, AddressSpace};
use crate::channel;
use crate::mm::{self, PAGE_SIZE, frame};
use crate::pipe;
use crate::sched::{self, SpawnError, ThreadId};
use crate::shared::Segment;
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
    /// The arguments exceed [`ARGS_MAX`] bytes.
    ArgumentsTooLong,
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
            Self::ArgumentsTooLong => write!(f, "argument list too long"),
            Self::Map(e) => write!(f, "cannot build address space: {e}"),
            Self::Spawn(e) => write!(f, "cannot create thread: {e:?}"),
        }
    }
}

/// An open file: a path and a position in it.
///
/// Files are identified by path rather than by inode, so renaming or
/// removing a file while it is open affects the descriptor. That is a
/// known simplification.
#[derive(Debug, Clone)]
pub struct OpenFile {
    /// Normalised absolute path.
    pub path: String,
    /// Position of the next read or write.
    pub offset: u64,
    /// May be read.
    pub readable: bool,
    /// May be written.
    pub writable: bool,
    /// Every write goes to the end of the file.
    pub append: bool,
}

/// What a descriptor refers to.
#[derive(Clone)]
pub enum Descriptor {
    /// A file in the filesystem.
    File(OpenFile),
    /// The reading end of a pipe.
    PipeRead(pipe::Reader),
    /// The writing end of a pipe.
    PipeWrite(pipe::Writer),
    /// One end of a channel connection: a pipe in each direction.
    Channel(pipe::Reader, pipe::Writer),
    /// A channel name being listened on.
    Listener(Arc<channel::Handle>),
    /// A UDP socket.
    Udp(Arc<crate::net::UdpSocket>),
    /// A TCP connection.
    Tcp(Arc<crate::net::tcp::TcpHandle>),
    /// A TCP port being listened on.
    TcpListener(Arc<crate::net::tcp::TcpListener>),
}

/// The standard streams of a new process: input, output, error. `None`
/// means the console.
pub type Stdio = [Option<Descriptor>; 3];

struct Record {
    /// The process's threads, the first one first. Finished threads stay
    /// listed; a process is over when none of them is alive.
    threads: Vec<ThreadId>,
    /// Set when the process ends.
    exit_status: Option<i32>,
    /// Working directory: a normalised absolute path.
    cwd: String,
    /// Open descriptors, indexed by descriptor minus 3.
    files: Vec<Option<Descriptor>>,
    /// Descriptors 0-2: where standard input, output and error are
    /// connected instead of the console.
    stdio: Stdio,
    /// Next unused address in the `map` area.
    map_next: u64,
}

struct Table {
    processes: BTreeMap<Pid, Record>,
    next_pid: Pid,
}

static TABLE: SpinLock<Table> = SpinLock::new(Table {
    processes: BTreeMap::new(),
    next_pid: 1,
});

/// First descriptor number available to files.
const FIRST_FILE_FD: u64 = 3;

/// Where a new process starts executing.
struct Start {
    entry: u64,
    stack: u64,
    /// Passed to the entry point in RDI.
    argument: u64,
    /// For threads created by `thread_create`: where to store 1 (and wake
    /// futex waiters) when the thread has ended.
    exit_flag: u64,
}

/// Allocates a zeroed frame, fills its start with `data`, and maps it at
/// `virt`.
fn map_page(
    space: &mut AddressSpace,
    virt: u64,
    data: &[u8],
    writable: bool,
    executable: bool,
) -> Result<(), ExecError> {
    debug_assert!(data.len() <= PAGE_SIZE as usize);
    let phys = frame::alloc_zeroed(0).ok_or(ExecError::Map(MapError::OutOfMemory))?;
    if !data.is_empty() {
        let page = mm::phys_to_virt(phys, PAGE_SIZE).expect("frames are in the direct map");
        // SAFETY: the frame was just allocated and is referenced by nothing
        // else; `data` is at most one page long.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), page, data.len()) };
    }
    space.map(virt, phys, writable, executable).map_err(|e| {
        // SAFETY: the mapping failed, so the frame is still only ours.
        let _ = unsafe { frame::free(phys, 0) };
        ExecError::Map(e)
    })
}

/// Writes the program arguments at the top of the user stack and returns
/// the initial stack pointer.
///
/// Layout at the returned address, as 64-bit words: the argument count,
/// then one pointer per argument, then a null pointer. The strings
/// themselves (NUL-terminated) sit above.
fn push_arguments(space: &mut AddressSpace, args: &[String]) -> Result<u64, ExecError> {
    let strings: usize = args.iter().map(|arg| arg.len() + 1).sum();
    if strings > ARGS_MAX || args.iter().any(|arg| arg.as_bytes().contains(&0)) {
        return Err(ExecError::ArgumentsTooLong);
    }
    let fault = ExecError::Map(MapError::InvalidRange);

    let mut cursor = USER_STACK_TOP;
    let mut pointers = Vec::with_capacity(args.len());
    for arg in args {
        cursor -= arg.len() as u64 + 1;
        // The stack frames are zero-filled, which provides the terminator.
        if !space.write(cursor, arg.as_bytes()) {
            return Err(fault);
        }
        pointers.push(cursor);
    }

    // argc, the pointers and the null terminator, 16-byte aligned so the
    // stack pointer the program starts with satisfies the ABI.
    let words = args.len() as u64 + 2;
    let base = (cursor - words * 8) & !0xF;
    let mut ok = space.write(base, &(args.len() as u64).to_le_bytes());
    for (index, pointer) in pointers.iter().enumerate() {
        ok &= space.write(base + 8 * (index as u64 + 1), &pointer.to_le_bytes());
    }
    if ok { Ok(base) } else { Err(fault) }
}

/// Builds a user address space from an ELF image.
fn load(image: &[u8], args: &[String]) -> Result<(AddressSpace, Start), ExecError> {
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
            true,
            false,
        )?;
    }
    let stack = push_arguments(&mut space, args)?;
    Ok((
        space,
        Start {
            entry: elf.entry(),
            stack,
            argument: 0,
            exit_flag: 0,
        },
    ))
}

/// Kernel-side entry of a process thread: drops to user mode.
fn enter(start: usize) {
    // SAFETY: `start` is the pointer produced by `Box::into_raw` in
    // `spawn` or `spawn_thread`, passed to exactly one thread.
    let start = unsafe { Box::from_raw(start as *mut Start) };
    let (entry, stack, argument) = (start.entry, start.stack, start.argument);
    if start.exit_flag != 0 {
        EXIT_FLAGS
            .lock()
            .insert(sched::current_thread(), start.exit_flag);
    }
    drop(start);
    // SAFETY: this thread was created by `spawn_process` with the address
    // space built by `load`, which is active now. For the first thread,
    // `entry` is inside an executable user segment and `stack` in the
    // stack `load` mapped. For further threads both come from the program
    // itself: if they are wrong, the thread faults in user mode like any
    // other bad jump, and the process is terminated. The scheduler
    // registered this thread's kernel stack when it switched to it.
    unsafe {
        enter_user(
            entry,
            stack,
            u64::from(USER_CS),
            u64::from(USER_DS),
            argument,
        )
    }
}

/// Where each thread created by `thread_create` wants its end announced.
static EXIT_FLAGS: SpinLock<BTreeMap<ThreadId, u64>> = SpinLock::new(BTreeMap::new());

/// Starts the program in `image` as a new process called `name`, with
/// arguments `args` and working directory `cwd` (a normalised absolute
/// path).
///
/// `stdio` connects the program's standard streams to files or pipes
/// instead of the console.
pub fn spawn(
    name: &str,
    image: &[u8],
    args: &[String],
    cwd: &str,
    stdio: Stdio,
) -> Result<Pid, ExecError> {
    let (space, start) = load(image, args)?;
    let space = sched::SharedSpace::new(space);
    let start = Box::into_raw(Box::new(start));

    let mut table = TABLE.lock();
    let pid = table.next_pid;
    match sched::spawn_process(name, enter, start as usize, space, pid) {
        Ok(thread) => {
            table.next_pid += 1;
            table.processes.insert(
                pid,
                Record {
                    threads: alloc::vec![thread],
                    exit_status: None,
                    cwd: cwd.into(),
                    files: Vec::new(),
                    stdio,
                    map_next: USER_MAP_BASE,
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

/// Starts another thread in the calling process: it begins at `entry`
/// with `argument` in its first argument register and `stack` as its
/// stack pointer. When it ends, 1 is stored in the `u32` at `exit_flag`
/// (if non-zero) and futex waiters on that address are woken. Returns the
/// thread's ID.
pub fn spawn_thread(entry: u64, stack: u64, argument: u64, exit_flag: u64) -> Option<ThreadId> {
    let pid = sched::current_pid()?;
    let space = sched::current_space()?;
    let start = Box::into_raw(Box::new(Start {
        entry,
        stack,
        argument,
        exit_flag,
    }));
    match sched::spawn_process("thread", enter, start as usize, space, pid) {
        Ok(thread) => {
            if let Some(record) = TABLE.lock().processes.get_mut(&pid) {
                record.threads.push(thread);
            }
            Some(thread)
        }
        Err(_) => {
            // SAFETY: no thread was created, so the pointer was never
            // handed over and is still uniquely owned here.
            drop(unsafe { Box::from_raw(start) });
            None
        }
    }
}

/// Ends the calling thread only. If it is the last thread of its process,
/// the process ends with status 0.
pub fn exit_thread() -> ! {
    let Some(pid) = sched::current_pid() else {
        sched::exit()
    };
    if sched::live_threads(pid) <= 1 {
        exit(0);
    }
    announce_thread_end();
    sched::exit()
}

/// Tells whoever waits for the calling thread that it has ended: stores 1
/// in its exit flag and wakes futex waiters. The thread never returns to
/// user mode after this, so its stack may be released by the waiter.
fn announce_thread_end() {
    let flag = EXIT_FLAGS.lock().remove(&sched::current_thread());
    if let Some(address) = flag
        && sched::with_address_space(|space| space.write(address, &1u32.to_le_bytes()))
            == Some(true)
    {
        crate::futex::wake(address, usize::MAX);
    }
}

/// Ends the calling process with `status`: the calling thread ends now,
/// and every other thread of the process the next time it would return to
/// user mode. If several threads end the process, the first status wins.
pub fn exit(status: i32) -> ! {
    if let Some(pid) = sched::current_pid() {
        // If it owned the screen, the text console takes over again.
        crate::display::release(pid);
        sched::kill_other_threads(pid);
    }
    // Closing descriptors can wake other threads (the other end of a
    // pipe), so they are taken out under the lock and dropped after it.
    let closed = sched::current_pid().and_then(|pid| {
        let mut table = TABLE.lock();
        let record = table.processes.get_mut(&pid)?;
        if record.exit_status.is_none() {
            record.exit_status = Some(status);
        }
        Some((
            core::mem::take(&mut record.files),
            core::mem::take(&mut record.stdio),
        ))
    });
    drop(closed);
    EXIT_FLAGS.lock().remove(&sched::current_thread());
    sched::exit()
}

/// Ends the calling process if it has been killed. Called on the way back
/// to user mode.
pub fn exit_if_killed() {
    if sched::current_killed() {
        exit(EXIT_KILLED);
    }
}

/// Whether any thread of the process is still alive, and if so, one of
/// them.
fn live_thread(pid: Pid) -> Option<Option<ThreadId>> {
    let threads = TABLE.lock().processes.get(&pid)?.threads.clone();
    Some(threads.into_iter().find(|&thread| sched::is_alive(thread)))
}

/// Waits for process `pid` to end — all of its threads — and returns its
/// exit status, or `None` if there is no such process (or it was already
/// waited for).
pub fn wait(pid: Pid) -> Option<i32> {
    // Threads can be created while waiting, so look again after each one.
    while let Some(thread) = live_thread(pid)? {
        sched::join(thread);
    }
    let record = TABLE.lock().processes.remove(&pid)?;
    // A process can only end without a status if the kernel ended it.
    Some(record.exit_status.unwrap_or(EXIT_KILLED))
}

/// Like [`wait`], but does not block: `Some(None)` means process `pid` is
/// still running.
pub fn try_wait(pid: Pid) -> Option<Option<i32>> {
    if live_thread(pid)?.is_some() {
        return Some(None);
    }
    let record = TABLE.lock().processes.remove(&pid)?;
    Some(Some(record.exit_status.unwrap_or(EXIT_KILLED)))
}

/// Discards the records of finished processes that nobody waited for, such
/// as background jobs.
pub fn reap_finished() {
    TABLE
        .lock()
        .processes
        .retain(|_, record| record.threads.iter().any(|&thread| sched::is_alive(thread)));
}

/// Requests termination of process `pid`.
pub fn kill(pid: Pid) -> bool {
    sched::kill(pid)
}

/// The ID and name of the calling process, for diagnostics.
pub fn describe_current() -> (Pid, String) {
    let pid = sched::current_pid().unwrap_or(0);
    let name = sched::threads()
        .into_iter()
        .find(|thread| thread.pid == Some(pid))
        .map(|thread| thread.name)
        .unwrap_or_default();
    (pid, name)
}

/// Runs `f` on the calling process's record.
fn with_current<R>(f: impl FnOnce(&mut Record) -> R) -> Option<R> {
    let pid = sched::current_pid()?;
    TABLE.lock().processes.get_mut(&pid).map(f)
}

/// The calling process's working directory.
pub fn current_dir() -> Option<String> {
    with_current(|record| record.cwd.clone())
}

/// Adds `descriptor` to the calling process's descriptor table and
/// returns its number, or gives it back if the table is full.
pub fn install(descriptor: Descriptor) -> Result<u64, Descriptor> {
    let mut descriptor = Some(descriptor);
    let number = with_current(|record| {
        let slot = match record.files.iter().position(Option::is_none) {
            Some(slot) => slot,
            None if record.files.len() + (FIRST_FILE_FD as usize) < MAX_OPEN_FILES => {
                record.files.push(None);
                record.files.len() - 1
            }
            None => return None,
        };
        record.files[slot] = descriptor.take();
        Some(slot as u64 + FIRST_FILE_FD)
    })
    .flatten();
    match (number, descriptor) {
        (Some(number), _) => Ok(number),
        (None, Some(descriptor)) => Err(descriptor),
        // Not reachable: the descriptor is only taken when a slot exists.
        (None, None) => Ok(0),
    }
}

/// A copy of what the calling process's descriptor `fd` refers to. For
/// descriptors 0-2, `None` means the console.
pub fn descriptor(fd: u64) -> Option<Descriptor> {
    with_current(|record| match fd.checked_sub(FIRST_FILE_FD) {
        None => record.stdio[fd as usize].clone(),
        Some(slot) => record.files.get(usize::try_from(slot).ok()?)?.clone(),
    })
    .flatten()
}

/// Sets the position of the calling process's descriptor `fd`, if it
/// refers to a file.
pub fn set_offset(fd: u64, offset: u64) {
    with_current(|record| {
        let entry = match fd.checked_sub(FIRST_FILE_FD) {
            None => record.stdio.get_mut(fd as usize),
            Some(slot) => usize::try_from(slot)
                .ok()
                .and_then(|slot| record.files.get_mut(slot)),
        };
        if let Some(Some(Descriptor::File(file))) = entry {
            file.offset = offset;
        }
    });
}

/// Closes the calling process's descriptor `fd` (3 or higher); `false` if
/// it was not open.
pub fn close(fd: u64) -> bool {
    let Some(slot) = fd
        .checked_sub(FIRST_FILE_FD)
        .and_then(|slot| usize::try_from(slot).ok())
    else {
        return false;
    };
    // Dropped after the process table lock is released: closing a pipe
    // end wakes the threads waiting on the other end.
    let closed = with_current(|record| record.files.get_mut(slot).and_then(Option::take)).flatten();
    closed.is_some()
}

/// The standard streams of the calling process, for a program it starts.
pub fn stdio() -> Stdio {
    with_current(|record| record.stdio.clone()).unwrap_or_default()
}

/// Number of pages mapped or unmapped per acquisition of the scheduler
/// lock, to bound the time interrupts stay disabled.
const MAP_BATCH: u64 = 32;

/// Maps `length` bytes (rounded up to pages) of zeroed, writable memory
/// into the calling process at an address that is a multiple of `align`,
/// and returns that address. `None` if the request is invalid or memory is
/// exhausted; nothing stays mapped in that case.
pub fn map_memory(length: u64, align: u64) -> Option<u64> {
    if length == 0 || length > MAP_MAX || !align.is_power_of_two() || align > MAP_MAX {
        return None;
    }
    let align = align.max(PAGE_SIZE);
    let pages = length.div_ceil(PAGE_SIZE);
    let base = reserve_addresses(pages, align)?;

    let mut mapped = 0;
    while mapped < pages {
        let batch = (pages - mapped).min(MAP_BATCH);
        let ok = sched::with_address_space(|space| {
            (0..batch).all(|index| {
                let virt = base + (mapped + index) * PAGE_SIZE;
                frame::alloc_zeroed(0).is_some_and(|phys| {
                    let result = space.map(virt, phys, true, false);
                    if result.is_err() {
                        // SAFETY: the frame was not mapped, so it is still
                        // exclusively ours.
                        let _ = unsafe { frame::free(phys, 0) };
                    }
                    result.is_ok()
                })
            })
        });
        if ok != Some(true) {
            unmap_memory(base, (mapped + batch) * PAGE_SIZE);
            return None;
        }
        mapped += batch;
    }
    Some(base)
}

/// Reserves `pages` pages of the calling process's `map` area at a
/// multiple of `align`. The range is never reused, so a failure to fill
/// it only wastes address space.
fn reserve_addresses(pages: u64, align: u64) -> Option<u64> {
    with_current(|record| {
        let base = record.map_next.checked_next_multiple_of(align)?;
        let end = base.checked_add(pages * PAGE_SIZE)?;
        (end <= USER_MAP_END).then(|| {
            record.map_next = end;
            base
        })
    })?
}

/// Maps a shared memory segment, writable, into the calling process and
/// returns its address. The mapping holds a reference to the segment
/// until it is unmapped or the process ends.
pub fn map_shared(segment: Arc<Segment>) -> Option<u64> {
    let space = sched::current_space()?;
    let pages = segment.frames().len() as u64;
    let base = reserve_addresses(pages, PAGE_SIZE)?;
    let mut mapped = 0;
    while mapped < pages {
        let batch = (pages - mapped).min(MAP_BATCH);
        let ok = space.with(|space| {
            (0..batch).all(|index| {
                let page = mapped + index;
                space
                    .map_shared(base + page * PAGE_SIZE, segment.frames()[page as usize])
                    .is_ok()
            })
        });
        if !ok {
            // Shared pages are not freed by unmapping them.
            space.with(|space| {
                for page in 0..mapped + batch {
                    space.unmap(base + page * PAGE_SIZE);
                }
            });
            return None;
        }
        mapped += batch;
    }
    space.add_shared(base, segment);
    Some(base)
}

/// Unmaps and frees every mapped page of the calling process in
/// `[address, address + length)`, which must lie in the `map` area and be
/// page-aligned. Returns `false` for an invalid range.
pub fn unmap_memory(address: u64, length: u64) -> bool {
    let Some(end) = address.checked_add(length) else {
        return false;
    };
    if !address.is_multiple_of(PAGE_SIZE) || address < USER_MAP_BASE || end > USER_MAP_END {
        return false;
    }
    let pages = length.div_ceil(PAGE_SIZE);
    // A shared segment is unmapped as a whole or not at all, so that the
    // reference to it can be given up exactly when its last page goes.
    let Some(shared_space) = sched::current_space() else {
        return false;
    };
    let whole_segment = shared_space.shared_pages_at(address) == Some(pages);
    if !whole_segment && shared_space.overlaps_shared(address, end) {
        return false;
    }
    let mut done = 0;
    while done < pages {
        let batch = (pages - done).min(MAP_BATCH);
        sched::with_address_space(|space| {
            for index in 0..batch {
                if let Some(phys) = space.unmap(address + (done + index) * PAGE_SIZE) {
                    // SAFETY: the page was just unmapped from the only
                    // address space that referenced the frame.
                    let _ = unsafe { frame::free(phys, 0) };
                }
            }
        });
        done += batch;
    }
    if whole_segment {
        // The pages are gone from the page table; now the reference.
        drop(shared_space.take_shared(address));
    }
    true
}

/// Changes the access rights of the calling process's memory in
/// `[address, address + length)`, which must lie in the `map` area and be
/// page-aligned and fully mapped. Returns `false` otherwise, or for a
/// request to make memory both writable and executable; in that case some
/// pages may already have been changed.
pub fn protect_memory(address: u64, length: u64, writable: bool, executable: bool) -> bool {
    let Some(end) = address.checked_add(length) else {
        return false;
    };
    if (writable && executable)
        || !address.is_multiple_of(PAGE_SIZE)
        || address < USER_MAP_BASE
        || end > USER_MAP_END
        // Shared memory stays writable data: another process may hold a
        // writable mapping of the same frames.
        || sched::current_space().is_none_or(|space| space.overlaps_shared(address, end))
    {
        return false;
    }
    let pages = length.div_ceil(PAGE_SIZE);
    let mut done = 0;
    while done < pages {
        let batch = (pages - done).min(MAP_BATCH);
        let ok = sched::with_address_space(|space| {
            (0..batch).all(|index| {
                space.protect(address + (done + index) * PAGE_SIZE, writable, executable)
            })
        });
        if ok != Some(true) {
            return false;
        }
        done += batch;
    }
    true
}
