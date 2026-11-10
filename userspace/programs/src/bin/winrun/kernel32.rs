// SPDX-License-Identifier: GPL-2.0-only
//! The Windows API functions available to programs: a part of
//! `KERNEL32.dll`.
//!
//! Each function is written from its public documentation (Microsoft
//! Learn) and implemented with Omit OS system calls. A function is either
//! implemented for the cases described in its comment or absent — in
//! which case `winrun` refuses to start a program that needs it. Where a
//! documented feature cannot be provided, the function fails with a Win32
//! error code rather than pretending.
//!
//! Known differences from Windows:
//!
//! * Text passed to `...A` functions is UTF-8 (the process reports code
//!   page 65001).
//! * File names are case-sensitive; the directory tree is drive `C:`.
//! * A file is opened either for reading or for writing, not both, and
//!   there is no seeking.
//! * There is one thread, no environment, and no way to tell whether
//!   standard output is the console or a file; it is reported as console.
//! * Console input ends lines with `\n`, not `\r\n`.

#![allow(
    non_snake_case,
    reason = "the functions carry the names Windows programs import"
)]

use alloc::alloc::{alloc, alloc_zeroed, dealloc, realloc};
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::cell::{RefCell, UnsafeCell};
use core::ffi::{CStr, c_char, c_void};
use core::sync::atomic::{AtomicUsize, Ordering};

use omit_pe::Symbol;
use omit_rt::abi::{self, Errno, KIND_DIRECTORY, open, protect};
use omit_win32::error::*;
use omit_win32::handle::{Handle, INVALID_HANDLE, Table};
use omit_win32::path::{self, Target};
use omit_win32::text::{self, CP_UTF8};
use omit_win32::{cmdline, error};

use crate::loader;

type Bool = i32;
const TRUE: Bool = 1;
const FALSE: Bool = 0;

/// The memory was not accessible (`ERROR_NOACCESS`).
const ERROR_NOACCESS: u32 = 998;

const PAGE: usize = 4096;
/// Most objects a program can have open; files are further limited by
/// the kernel.
const MAX_HANDLES: usize = 256;

/// What a handle refers to.
enum Object {
    ConsoleInput,
    /// Console output through the given descriptor (standard output or
    /// standard error).
    ConsoleOutput(u64),
    /// The null device.
    Null,
    File {
        fd: u64,
        /// For size queries.
        path: String,
        writable: bool,
    },
}

struct State {
    handles: Table<Object>,
    standard: [Handle; 3],
    image_base: usize,
    command_line: Vec<u8>,
    command_line_wide: Vec<u16>,
    /// Regions handed out by `VirtualAlloc`: base and size.
    regions: Vec<(usize, usize)>,
}

/// Process-wide state. Omit OS processes have exactly one thread, and the
/// Windows program can only reach this state by calling the functions
/// below, so access is never concurrent; the `RefCell` turns accidental
/// re-entrancy into a panic.
struct Global(UnsafeCell<RefCell<Option<State>>>);

// SAFETY: see the type's documentation — a process has a single thread.
unsafe impl Sync for Global {}

static STATE: Global = Global(UnsafeCell::new(RefCell::new(None)));
/// Address of the thread environment block.
static TEB: AtomicUsize = AtomicUsize::new(0);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    // SAFETY: single-threaded (see `Global`); the reference does not
    // escape this function.
    let cell = unsafe { &*STATE.0.get() };
    let mut state = cell.borrow_mut();
    f(state.as_mut().expect("kernel32::init has run"))
}

/// What [`init`] needs to know about the loaded program.
pub struct Setup<'a> {
    pub image_base: usize,
    pub stack_top: usize,
    pub stack_limit: usize,
    /// The program's path followed by its arguments.
    pub arguments: &'a [&'a str],
}

// Offsets in the thread and process environment blocks, from the public
// structure definitions (winternl.h and the documented NT_TIB). Only the
// fields programs commonly read directly are filled in; the rest is zero.
const TEB_STACK_BASE: usize = 0x08;
const TEB_STACK_LIMIT: usize = 0x10;
const TEB_SELF: usize = 0x30;
const TEB_PROCESS_ID: usize = 0x40;
const TEB_THREAD_ID: usize = 0x48;
const TEB_PEB: usize = 0x60;
const TEB_LAST_ERROR: usize = 0x68;
const PEB_IMAGE_BASE: usize = 0x10;

/// Prepares the process for running a Windows program: standard handles,
/// command line, and the thread environment block that Windows code
/// expects to find through the `GS` register.
pub fn init(setup: Setup<'_>) -> Result<(), Errno> {
    let mut handles = Table::new(MAX_HANDLES);
    let mut standard = [0; 3];
    let objects = [
        Object::ConsoleInput,
        Object::ConsoleOutput(abi::STDOUT),
        Object::ConsoleOutput(abi::STDERR),
    ];
    for (slot, object) in standard.iter_mut().zip(objects) {
        *slot = handles.insert(object).map_err(|_| Errno::NoSpace)?;
    }

    let line = cmdline::build(setup.arguments);
    let command_line_wide = text::to_utf16z(&line);
    let mut command_line = line.into_bytes();
    command_line.push(0);

    // Two pages for the TEB, one for the PEB.
    let block = omit_rt::map(3 * PAGE, PAGE)? as usize;
    let peb = block + 2 * PAGE;
    let pid = omit_rt::getpid() as usize;
    let fields = [
        (block + TEB_STACK_BASE, setup.stack_top),
        (block + TEB_STACK_LIMIT, setup.stack_limit),
        (block + TEB_SELF, block),
        (block + TEB_PROCESS_ID, pid),
        (block + TEB_THREAD_ID, pid),
        (block + TEB_PEB, peb),
        (peb + PEB_IMAGE_BASE, setup.image_base),
    ];
    for (address, value) in fields {
        // SAFETY: every address lies inside the three pages just mapped,
        // which are writable and used by nothing else, and is 8-byte
        // aligned.
        unsafe { (address as *mut usize).write(value) };
    }
    omit_rt::set_thread_pointer(block)?;
    TEB.store(block, Ordering::Relaxed);

    // SAFETY: single-threaded (see `Global`); no borrow is active.
    let cell = unsafe { &*STATE.0.get() };
    *cell.borrow_mut() = Some(State {
        handles,
        standard,
        image_base: setup.image_base,
        command_line,
        command_line_wide,
        regions: Vec::new(),
    });
    Ok(())
}

fn last_error_slot() -> *mut u32 {
    (TEB.load(Ordering::Relaxed) + TEB_LAST_ERROR) as *mut u32
}

fn set_last_error(code: u32) {
    // SAFETY: the TEB was mapped by `init` and stays mapped; the field is
    // 4-byte aligned. Windows keeps the last error in the same place.
    unsafe { last_error_slot().write(code) };
}

fn fail(code: u32) -> Bool {
    set_last_error(code);
    FALSE
}

fn fail_errno(errno: Errno) -> Bool {
    fail(error::from_errno(errno))
}

/// The text of a NUL-terminated `...A` string argument.
///
/// # Safety
///
/// `pointer` must be null or point to a NUL-terminated string that stays
/// unchanged for `'a`.
unsafe fn text_a<'a>(pointer: *const c_char) -> Result<&'a str, u32> {
    if pointer.is_null() {
        return Err(ERROR_INVALID_PARAMETER);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map_err(|_| ERROR_INVALID_NAME)
}

/// The text of a NUL-terminated `...W` string argument.
///
/// # Safety
///
/// `pointer` must be null or point to a NUL-terminated UTF-16 string.
unsafe fn text_w(pointer: *const u16) -> Result<String, u32> {
    if pointer.is_null() {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut length = 0;
    // SAFETY: the caller guarantees a terminator, so every unit up to it
    // is readable.
    while unsafe { pointer.add(length).read_unaligned() } != 0 {
        length += 1;
    }
    let mut units = Vec::with_capacity(length);
    for index in 0..length {
        // SAFETY: `index` is before the terminator found above.
        units.push(unsafe { pointer.add(index).read_unaligned() });
    }
    Ok(text::from_utf16(&units))
}

/// Translates a path argument to an Omit OS path; devices are rejected.
fn file_path(name: Result<&str, u32>) -> Result<String, u32> {
    match path::translate(name?)? {
        Target::File(path) => Ok(path),
        Target::Console | Target::Null => Err(ERROR_ACCESS_DENIED),
    }
}

// ---------------------------------------------------------------------
// Process
// ---------------------------------------------------------------------

/// `ExitProcess`: ends the process with the given exit code.
extern "win64" fn ExitProcess(code: u32) -> ! {
    omit_rt::exit(code as i32)
}

/// `GetCurrentProcess`: the pseudo-handle for the current process.
extern "win64" fn GetCurrentProcess() -> Handle {
    INVALID_HANDLE
}

/// `GetCurrentProcessId`.
extern "win64" fn GetCurrentProcessId() -> u32 {
    omit_rt::getpid() as u32
}

/// `GetCurrentThreadId`: the process has one thread; its ID is the
/// process ID.
extern "win64" fn GetCurrentThreadId() -> u32 {
    omit_rt::getpid() as u32
}

/// `GetLastError`.
extern "win64" fn GetLastError() -> u32 {
    // SAFETY: see `set_last_error`.
    unsafe { last_error_slot().read() }
}

/// `SetLastError`.
extern "win64" fn SetLastError(code: u32) {
    set_last_error(code);
}

/// `GetCommandLineA`.
extern "win64" fn GetCommandLineA() -> *const c_char {
    with_state(|state| state.command_line.as_ptr().cast())
}

/// `GetCommandLineW`.
extern "win64" fn GetCommandLineW() -> *const u16 {
    with_state(|state| state.command_line_wide.as_ptr())
}

// ---------------------------------------------------------------------
// Modules (the program and its libraries)
// ---------------------------------------------------------------------

/// The module handle of `KERNEL32.dll`: the address of this marker. The
/// library is part of this program rather than a loaded image.
static KERNEL32_MODULE: u8 = 0;

fn kernel32_module() -> usize {
    &raw const KERNEL32_MODULE as usize
}

fn is_kernel32(name: &str) -> bool {
    name.eq_ignore_ascii_case("kernel32.dll") || name.eq_ignore_ascii_case("kernel32")
}

/// Turns a library name as Windows programs write it into what the
/// loader takes: a bare file name, or an Omit OS path if a directory was
/// given.
fn library_name(name: &str) -> Result<String, u32> {
    if name.contains(['\\', '/', ':']) {
        file_path(Ok(name))
    } else {
        Ok(name.into())
    }
}

fn module_handle(name: Result<&str, u32>) -> usize {
    let found = name.and_then(|name| {
        if is_kernel32(name) {
            return Ok(kernel32_module());
        }
        loader::find(&library_name(name)?).ok_or(ERROR_MOD_NOT_FOUND)
    });
    found.unwrap_or_else(|code| {
        set_last_error(code);
        0
    })
}

/// `GetModuleHandleA`: the handle (base address) of the program itself
/// for a null name, or of a library that is already loaded.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe extern "win64" fn GetModuleHandleA(name: *const c_char) -> usize {
    if name.is_null() {
        return with_state(|state| state.image_base);
    }
    // SAFETY: guaranteed by the caller.
    module_handle(unsafe { text_a(name) })
}

/// `GetModuleHandleW`: see [`GetModuleHandleA`].
///
/// # Safety
///
/// `name` must be null or a NUL-terminated UTF-16 string.
unsafe extern "win64" fn GetModuleHandleW(name: *const u16) -> usize {
    if name.is_null() {
        return with_state(|state| state.image_base);
    }
    // SAFETY: guaranteed by the caller.
    let name = unsafe { text_w(name) };
    module_handle(name.as_deref().map_err(|&code| code))
}

/// `LoadLibraryA`: loads a library file and the libraries it needs, runs
/// their initialisation routines, and returns the library's handle. A
/// name without a directory is looked for in `/bin`; `.dll` is assumed if
/// there is no extension. Loading an already loaded library returns the
/// same handle.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe extern "win64" fn LoadLibraryA(name: *const c_char) -> usize {
    // SAFETY: guaranteed by the caller.
    let name = unsafe { text_a(name) };
    let loaded = name.and_then(|name| {
        if is_kernel32(name) {
            return Ok(kernel32_module());
        }
        let (base, initialisers) =
            loader::load_library(&library_name(name)?).map_err(|error| match error {
                loader::LoadError::Unusable(_) => ERROR_BAD_EXE_FORMAT,
                loader::LoadError::Missing(items) => {
                    if items
                        .iter()
                        .any(|item| item.ends_with("(library not found)"))
                    {
                        ERROR_MOD_NOT_FOUND
                    } else {
                        ERROR_PROC_NOT_FOUND
                    }
                }
            })?;
        for library in initialisers {
            // SAFETY: the loader validated, mapped, relocated and bound
            // the library; its entry point has the `DllMain` signature.
            // The third argument is null for libraries loaded on request.
            let ok = unsafe {
                let entry: extern "win64" fn(usize, u32, usize) -> Bool =
                    core::mem::transmute(library.entry);
                entry(library.base, crate::DLL_PROCESS_ATTACH as u32, 0)
            };
            if ok == FALSE {
                // The library stays mapped (libraries are never unloaded)
                // but the caller is told it cannot be used.
                return Err(ERROR_DLL_INIT_FAILED);
            }
        }
        Ok(base)
    });
    loaded.unwrap_or_else(|code| {
        set_last_error(code);
        0
    })
}

/// `FreeLibrary`: succeeds for a loaded library. Libraries stay loaded
/// until the process ends.
extern "win64" fn FreeLibrary(module: usize) -> Bool {
    if module == kernel32_module() || loader::path_of(module).is_some() {
        TRUE
    } else {
        fail(ERROR_MOD_NOT_FOUND)
    }
}

/// `GetProcAddress`: the address of a function exported by a module, by
/// name or (for loaded libraries) by ordinal.
///
/// # Safety
///
/// `name` must be an ordinal (a value below 65536) or a NUL-terminated
/// string.
unsafe extern "win64" fn GetProcAddress(module: usize, name: *const c_char) -> usize {
    let symbol = if (name as usize) < 0x1_0000 {
        Ok(Symbol::Ordinal(name as usize as u16))
    } else {
        // SAFETY: not an ordinal, so the caller guarantees a string.
        unsafe { text_a(name) }.map(Symbol::Name)
    };
    let address = symbol.and_then(|symbol| {
        if module == kernel32_module() {
            resolve(symbol).ok_or(ERROR_PROC_NOT_FOUND)
        } else if loader::path_of(module).is_none() {
            Err(ERROR_MOD_NOT_FOUND)
        } else {
            loader::export(module, symbol).ok_or(ERROR_PROC_NOT_FOUND)
        }
    });
    address.unwrap_or_else(|code| {
        set_last_error(code);
        0
    })
}

/// `GetModuleFileNameA`: the path of a module's file, in Windows form
/// (`C:\bin\tool.exe`); a null handle means the program. Returns the
/// number of bytes stored, not counting the terminator; if the buffer is
/// too small the path is truncated and the buffer size returned, with
/// `ERROR_INSUFFICIENT_BUFFER`.
///
/// # Safety
///
/// `buffer` must be valid for writing `size` bytes.
unsafe extern "win64" fn GetModuleFileNameA(module: usize, buffer: *mut u8, size: u32) -> u32 {
    let base = if module == 0 {
        with_state(|state| state.image_base)
    } else {
        module
    };
    let Some(native) = loader::path_of(base) else {
        set_last_error(ERROR_MOD_NOT_FOUND);
        return 0;
    };
    if buffer.is_null() || size == 0 {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    let path = path::to_windows(&native);
    let size = size as usize;
    let copied = path.len().min(size - 1);
    // SAFETY: `copied + 1 <= size` bytes are written to a buffer the
    // caller guarantees to be that large.
    unsafe {
        core::ptr::copy_nonoverlapping(path.as_ptr(), buffer, copied);
        buffer.add(copied).write(0);
    }
    if copied < path.len() {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return size as u32;
    }
    copied as u32
}

/// `IsDebuggerPresent`: there is no debugger.
extern "win64" fn IsDebuggerPresent() -> Bool {
    FALSE
}

/// `GetEnvironmentVariableA`: there is no environment, so no variable is
/// ever found.
extern "win64" fn GetEnvironmentVariableA(
    _name: *const c_char,
    _buffer: *mut c_char,
    _size: u32,
) -> u32 {
    set_last_error(ERROR_ENVVAR_NOT_FOUND);
    0
}

// ---------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------

/// `Sleep`. `INFINITE` never returns.
extern "win64" fn Sleep(milliseconds: u32) {
    if milliseconds == u32::MAX {
        loop {
            omit_rt::sleep_ms(60_000);
        }
    }
    if milliseconds == 0 {
        omit_rt::yield_now();
    } else {
        omit_rt::sleep_ms(u64::from(milliseconds));
    }
}

/// `GetTickCount64`: milliseconds since the system started.
extern "win64" fn GetTickCount64() -> u64 {
    omit_rt::uptime_ms()
}

/// `GetTickCount`.
extern "win64" fn GetTickCount() -> u32 {
    omit_rt::uptime_ms() as u32
}

/// `QueryPerformanceFrequency`: the counter below counts milliseconds.
///
/// # Safety
///
/// `frequency` must be null or valid for a write.
unsafe extern "win64" fn QueryPerformanceFrequency(frequency: *mut i64) -> Bool {
    if frequency.is_null() {
        return fail(ERROR_NOACCESS);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { frequency.write_unaligned(1000) };
    TRUE
}

/// `QueryPerformanceCounter`.
///
/// # Safety
///
/// `counter` must be null or valid for a write.
unsafe extern "win64" fn QueryPerformanceCounter(counter: *mut i64) -> Bool {
    if counter.is_null() {
        return fail(ERROR_NOACCESS);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { counter.write_unaligned(omit_rt::uptime_ms() as i64) };
    TRUE
}

// ---------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------

const HEAP_ZERO_MEMORY: u32 = 0x8;
/// Windows guarantees this alignment for heap blocks on 64-bit systems.
const HEAP_ALIGN: usize = 16;
/// Bytes in front of each block, holding its size.
const HEAP_HEADER: usize = 16;

/// The process heap's handle: the address of this marker.
static PROCESS_HEAP: u8 = 0;

fn process_heap() -> Handle {
    &raw const PROCESS_HEAP as Handle
}

fn heap_layout(size: usize) -> Option<Layout> {
    Layout::from_size_align(size.checked_add(HEAP_HEADER)?, HEAP_ALIGN).ok()
}

/// `GetProcessHeap`.
extern "win64" fn GetProcessHeap() -> Handle {
    process_heap()
}

/// `HeapAlloc` on the process heap. Honours `HEAP_ZERO_MEMORY`; returns
/// null when memory is exhausted.
extern "win64" fn HeapAlloc(heap: Handle, flags: u32, size: usize) -> *mut u8 {
    let Some(layout) = heap_layout(size) else {
        return core::ptr::null_mut();
    };
    if heap != process_heap() {
        set_last_error(ERROR_INVALID_HANDLE);
        return core::ptr::null_mut();
    }
    // SAFETY: the layout has a non-zero size (it includes the header).
    let block = unsafe {
        if flags & HEAP_ZERO_MEMORY != 0 {
            alloc_zeroed(layout)
        } else {
            alloc(layout)
        }
    };
    if block.is_null() {
        return block;
    }
    // SAFETY: the block is at least `HEAP_HEADER` bytes and aligned for
    // `usize`; the returned pointer stays inside the allocation.
    unsafe {
        block.cast::<usize>().write(size);
        block.add(HEAP_HEADER)
    }
}

/// The start of the allocation and the size of the block at `pointer`.
///
/// # Safety
///
/// `pointer` must have been returned by `HeapAlloc` or `HeapReAlloc` and
/// not freed.
unsafe fn heap_block(pointer: *mut u8) -> (*mut u8, usize) {
    // SAFETY: per the contract, a header precedes the block.
    unsafe {
        let start = pointer.sub(HEAP_HEADER);
        (start, start.cast::<usize>().read())
    }
}

/// `HeapFree`. Freeing a null pointer succeeds.
///
/// # Safety
///
/// `pointer` must be null or a live block of the process heap.
unsafe extern "win64" fn HeapFree(heap: Handle, _flags: u32, pointer: *mut u8) -> Bool {
    if heap != process_heap() {
        return fail(ERROR_INVALID_HANDLE);
    }
    if pointer.is_null() {
        return TRUE;
    }
    // SAFETY: the caller guarantees a live block; its layout is the one
    // `HeapAlloc` computed from the recorded size.
    unsafe {
        let (start, size) = heap_block(pointer);
        dealloc(
            start,
            Layout::from_size_align_unchecked(size + HEAP_HEADER, HEAP_ALIGN),
        );
    }
    TRUE
}

/// `HeapReAlloc`. Honours `HEAP_ZERO_MEMORY` for the added part. The
/// "in place only" flag is not supported: the block may move.
///
/// # Safety
///
/// `pointer` must be a live block of the process heap.
unsafe extern "win64" fn HeapReAlloc(
    heap: Handle,
    flags: u32,
    pointer: *mut u8,
    size: usize,
) -> *mut u8 {
    const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;
    let layout = heap_layout(size);
    if heap != process_heap() || pointer.is_null() || flags & HEAP_REALLOC_IN_PLACE_ONLY != 0 {
        set_last_error(ERROR_INVALID_PARAMETER);
        return core::ptr::null_mut();
    }
    let Some(layout) = layout else {
        return core::ptr::null_mut();
    };
    // SAFETY: the caller guarantees a live block; the old layout is the
    // one it was allocated with, and the new size was validated above.
    unsafe {
        let (start, old_size) = heap_block(pointer);
        let old_layout = Layout::from_size_align_unchecked(old_size + HEAP_HEADER, HEAP_ALIGN);
        let block = realloc(start, old_layout, layout.size());
        if block.is_null() {
            return block;
        }
        block.cast::<usize>().write(size);
        let data = block.add(HEAP_HEADER);
        if flags & HEAP_ZERO_MEMORY != 0 && size > old_size {
            data.add(old_size).write_bytes(0, size - old_size);
        }
        data
    }
}

/// `HeapSize`.
///
/// # Safety
///
/// `pointer` must be a live block of the process heap.
unsafe extern "win64" fn HeapSize(heap: Handle, _flags: u32, pointer: *const u8) -> usize {
    if heap != process_heap() || pointer.is_null() {
        set_last_error(ERROR_INVALID_PARAMETER);
        return usize::MAX;
    }
    // SAFETY: guaranteed by the caller.
    unsafe { heap_block(pointer.cast_mut()).1 }
}

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_RELEASE: u32 = 0x8000;
const PAGE_READONLY: u32 = 0x02;
const PAGE_READWRITE: u32 = 0x04;
const PAGE_EXECUTE_READ: u32 = 0x20;

/// `VirtualAlloc`: allocates zeroed pages at an address the system
/// chooses, a multiple of 64 KiB. Supported: reserving and committing in
/// one step, with read-only, read/write or execute/read access. Not
/// supported (the call fails): a requested address, reserving without
/// committing, and memory that is writable and executable at once.
extern "win64" fn VirtualAlloc(
    address: *mut c_void,
    size: usize,
    kind: u32,
    protection: u32,
) -> *mut c_void {
    let refuse = |code| {
        set_last_error(code);
        core::ptr::null_mut()
    };
    if size == 0 {
        return refuse(ERROR_INVALID_PARAMETER);
    }
    if !address.is_null() {
        return refuse(ERROR_INVALID_ADDRESS);
    }
    if kind & MEM_COMMIT == 0 || kind & !(MEM_COMMIT | MEM_RESERVE) != 0 {
        return refuse(ERROR_NOT_SUPPORTED);
    }
    let rights = match protection {
        PAGE_READWRITE => protect::WRITE,
        PAGE_READONLY => protect::READ,
        PAGE_EXECUTE_READ => protect::EXECUTE,
        _ => return refuse(ERROR_NOT_SUPPORTED),
    };
    let Some(size) = size.checked_next_multiple_of(PAGE) else {
        return refuse(ERROR_NOT_ENOUGH_MEMORY);
    };
    let base = match omit_rt::map(size, 0x1_0000) {
        Ok(base) => base,
        Err(_) => return refuse(ERROR_NOT_ENOUGH_MEMORY),
    };
    if rights != protect::WRITE {
        // SAFETY: the memory was just mapped; nothing refers to it.
        let changed = unsafe { omit_rt::protect(base, size, rights) };
        if changed.is_err() {
            // SAFETY: as above.
            let _ = unsafe { omit_rt::unmap(base, size) };
            return refuse(ERROR_NOT_ENOUGH_MEMORY);
        }
    }
    with_state(|state| state.regions.push((base as usize, size)));
    base.cast()
}

/// `VirtualFree` with `MEM_RELEASE`: returns a whole region obtained from
/// `VirtualAlloc`. Decommitting part of a region is not supported.
///
/// # Safety
///
/// Nothing may use the region afterwards.
unsafe extern "win64" fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> Bool {
    if kind != MEM_RELEASE {
        return fail(ERROR_NOT_SUPPORTED);
    }
    if size != 0 {
        // Documented requirement for MEM_RELEASE.
        return fail(ERROR_INVALID_PARAMETER);
    }
    let region = with_state(|state| {
        let index = state
            .regions
            .iter()
            .position(|&(base, _)| base == address as usize)?;
        Some(state.regions.swap_remove(index))
    });
    match region {
        // SAFETY: the region was mapped by `VirtualAlloc` with this size;
        // the caller guarantees it is no longer used.
        Some((base, size)) => match unsafe { omit_rt::unmap(base as *mut u8, size) } {
            Ok(()) => TRUE,
            Err(errno) => fail_errno(errno),
        },
        None => fail(ERROR_INVALID_ADDRESS),
    }
}

// ---------------------------------------------------------------------
// Handles, console and files
// ---------------------------------------------------------------------

const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const STD_ERROR_HANDLE: u32 = -12i32 as u32;

/// `GetStdHandle`.
extern "win64" fn GetStdHandle(which: u32) -> Handle {
    let index = match which {
        STD_INPUT_HANDLE => 0,
        STD_OUTPUT_HANDLE => 1,
        STD_ERROR_HANDLE => 2,
        _ => {
            set_last_error(ERROR_INVALID_HANDLE);
            return INVALID_HANDLE;
        }
    };
    with_state(|state| state.standard[index])
}

/// `CloseHandle`.
extern "win64" fn CloseHandle(handle: Handle) -> Bool {
    match with_state(|state| state.handles.remove(handle)) {
        Some(Object::File { fd, .. }) => match omit_rt::close(fd) {
            Ok(()) => TRUE,
            Err(errno) => fail_errno(errno),
        },
        Some(_) => TRUE,
        None => fail(ERROR_INVALID_HANDLE),
    }
}

/// Where a write to a handle goes.
enum Sink {
    Descriptor(u64),
    Discard,
}

fn sink(handle: Handle, console_only: bool) -> Result<Sink, u32> {
    with_state(|state| match state.handles.get(handle) {
        Some(Object::ConsoleOutput(fd)) => Ok(Sink::Descriptor(*fd)),
        Some(Object::File {
            fd, writable: true, ..
        }) if !console_only => Ok(Sink::Descriptor(*fd)),
        Some(Object::Null) if !console_only => Ok(Sink::Discard),
        Some(Object::File { .. } | Object::ConsoleInput) if !console_only => {
            Err(ERROR_ACCESS_DENIED)
        }
        _ => Err(ERROR_INVALID_HANDLE),
    })
}

fn write_to(sink: &Sink, bytes: &[u8]) -> Result<(), u32> {
    match sink {
        Sink::Descriptor(fd) => omit_rt::write_all(*fd, bytes).map_err(error::from_errno),
        Sink::Discard => Ok(()),
    }
}

/// `WriteFile`: writes to the console, a file opened for writing, or the
/// null device. Always writes everything or fails. Positioned writes
/// (a non-null `overlapped`) are not supported.
///
/// # Safety
///
/// `buffer` must be valid for reading `length` bytes; `written` must be
/// null or valid for a write.
unsafe extern "win64" fn WriteFile(
    handle: Handle,
    buffer: *const u8,
    length: u32,
    written: *mut u32,
    overlapped: *mut c_void,
) -> Bool {
    if !written.is_null() {
        // SAFETY: guaranteed by the caller.
        unsafe { written.write_unaligned(0) };
    }
    if !overlapped.is_null() {
        return fail(ERROR_NOT_SUPPORTED);
    }
    let sink = match sink(handle, false) {
        Ok(sink) => sink,
        Err(code) => return fail(code),
    };
    if length == 0 {
        return TRUE;
    }
    if buffer.is_null() {
        return fail(ERROR_NOACCESS);
    }
    // SAFETY: the caller guarantees `length` readable bytes.
    let bytes = unsafe { core::slice::from_raw_parts(buffer, length as usize) };
    if let Err(code) = write_to(&sink, bytes) {
        return fail(code);
    }
    if !written.is_null() {
        // SAFETY: guaranteed by the caller.
        unsafe { written.write_unaligned(length) };
    }
    TRUE
}

/// `WriteConsoleA`: writes text to a console handle.
///
/// # Safety
///
/// As for [`WriteFile`].
unsafe extern "win64" fn WriteConsoleA(
    handle: Handle,
    buffer: *const u8,
    length: u32,
    written: *mut u32,
    _reserved: *mut c_void,
) -> Bool {
    if let Err(code) = sink(handle, true) {
        return fail(code);
    }
    // SAFETY: same contract; a console handle was verified above.
    unsafe { WriteFile(handle, buffer, length, written, core::ptr::null_mut()) }
}

/// `WriteConsoleW`: writes UTF-16 text to a console handle; `length` and
/// `written` count UTF-16 units.
///
/// # Safety
///
/// `buffer` must be valid for reading `length` UTF-16 units; `written`
/// must be null or valid for a write.
unsafe extern "win64" fn WriteConsoleW(
    handle: Handle,
    buffer: *const u16,
    length: u32,
    written: *mut u32,
    _reserved: *mut c_void,
) -> Bool {
    let sink = match sink(handle, true) {
        Ok(sink) => sink,
        Err(code) => return fail(code),
    };
    if buffer.is_null() && length != 0 {
        return fail(ERROR_NOACCESS);
    }
    let mut units = Vec::with_capacity(length as usize);
    for index in 0..length as usize {
        // SAFETY: the caller guarantees `length` readable units.
        units.push(unsafe { buffer.add(index).read_unaligned() });
    }
    if let Err(code) = write_to(&sink, text::from_utf16(&units).as_bytes()) {
        return fail(code);
    }
    if !written.is_null() {
        // SAFETY: guaranteed by the caller.
        unsafe { written.write_unaligned(length) };
    }
    TRUE
}

/// `ReadFile`: reads from the console (one edited line at a time), a
/// file opened for reading, or the null device. At the end of the data
/// it succeeds with zero bytes read, as Windows does.
///
/// # Safety
///
/// `buffer` must be valid for writing `length` bytes; `read` must be null
/// or valid for a write.
unsafe extern "win64" fn ReadFile(
    handle: Handle,
    buffer: *mut u8,
    length: u32,
    read: *mut u32,
    overlapped: *mut c_void,
) -> Bool {
    if !read.is_null() {
        // SAFETY: guaranteed by the caller.
        unsafe { read.write_unaligned(0) };
    }
    if !overlapped.is_null() {
        return fail(ERROR_NOT_SUPPORTED);
    }
    let source = with_state(|state| match state.handles.get(handle) {
        Some(Object::ConsoleInput) => Ok(Some(abi::STDIN)),
        Some(Object::File {
            fd,
            writable: false,
            ..
        }) => Ok(Some(*fd)),
        Some(Object::Null) => Ok(None),
        Some(_) => Err(ERROR_ACCESS_DENIED),
        None => Err(ERROR_INVALID_HANDLE),
    });
    let fd = match source {
        Ok(Some(fd)) => fd,
        Ok(None) => return TRUE,
        Err(code) => return fail(code),
    };
    if length == 0 {
        return TRUE;
    }
    if buffer.is_null() {
        return fail(ERROR_NOACCESS);
    }
    // SAFETY: the caller guarantees `length` writable bytes; the memory
    // may be uninitialised, which is fine for bytes that are only written.
    let bytes = unsafe { core::slice::from_raw_parts_mut(buffer, length as usize) };
    match omit_rt::read(fd, bytes) {
        Ok(count) => {
            if !read.is_null() {
                // SAFETY: guaranteed by the caller.
                unsafe { read.write_unaligned(count as u32) };
            }
            TRUE
        }
        Err(errno) => fail_errno(errno),
    }
}

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const FILE_READ_DATA: u32 = 0x1;
const FILE_WRITE_DATA: u32 = 0x2;
const FILE_APPEND_DATA: u32 = 0x4;

const CREATE_NEW: u32 = 1;
const CREATE_ALWAYS: u32 = 2;
const OPEN_EXISTING: u32 = 3;
const OPEN_ALWAYS: u32 = 4;
const TRUNCATE_EXISTING: u32 = 5;

fn create_file(name: Result<&str, u32>, access: u32, disposition: u32) -> Result<Handle, u32> {
    let reading = access & (GENERIC_READ | GENERIC_ALL | FILE_READ_DATA) != 0;
    let writing = access & (GENERIC_WRITE | GENERIC_ALL | FILE_WRITE_DATA) != 0;
    let appending = access & FILE_APPEND_DATA != 0;

    let object = match path::translate(name?)? {
        Target::Null => Object::Null,
        Target::Console if writing || appending => Object::ConsoleOutput(abi::STDOUT),
        Target::Console => Object::ConsoleInput,
        Target::File(path) => {
            if reading && (writing || appending) {
                // The kernel opens files for reading or for writing.
                return Err(ERROR_NOT_SUPPORTED);
            }
            let existed = omit_rt::stat(&path).is_ok();
            let change = match disposition {
                CREATE_NEW if existed => return Err(ERROR_FILE_EXISTS),
                CREATE_NEW | OPEN_ALWAYS => open::CREATE,
                CREATE_ALWAYS => open::CREATE | open::TRUNCATE,
                OPEN_EXISTING => 0,
                TRUNCATE_EXISTING if !writing => return Err(ERROR_INVALID_PARAMETER),
                TRUNCATE_EXISTING => open::TRUNCATE,
                _ => return Err(ERROR_INVALID_PARAMETER),
            };
            let writable = writing || appending;
            let fd = if writable {
                let mode = if writing { 0 } else { open::APPEND };
                omit_rt::open(&path, open::WRITE | change | mode)
            } else {
                if change != 0 {
                    // Creating or emptying a file that is then read: do
                    // that part through a short-lived writable descriptor.
                    let fd =
                        omit_rt::open(&path, open::WRITE | change).map_err(error::from_errno)?;
                    let _ = omit_rt::close(fd);
                }
                omit_rt::open(&path, open::READ)
            }
            .map_err(error::from_errno)?;
            // Documented: these two report that the file already existed
            // even though the call succeeds.
            set_last_error(
                if existed && matches!(disposition, CREATE_ALWAYS | OPEN_ALWAYS) {
                    ERROR_ALREADY_EXISTS
                } else {
                    ERROR_SUCCESS
                },
            );
            Object::File { fd, path, writable }
        }
    };
    with_state(|state| state.handles.insert(object)).map_err(|object| {
        if let Object::File { fd, .. } = object {
            let _ = omit_rt::close(fd);
        }
        ERROR_TOO_MANY_OPEN_FILES
    })
}

fn handle_or_invalid(result: Result<Handle, u32>) -> Handle {
    result.unwrap_or_else(|code| {
        set_last_error(code);
        INVALID_HANDLE
    })
}

/// `CreateFileA`: opens or creates a file, the console (`CON`) or the
/// null device (`NUL`). All five creation dispositions are implemented.
/// Sharing modes, security attributes, flags and the template file are
/// ignored. Opening a file for both reading and writing is not
/// supported.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe extern "win64" fn CreateFileA(
    name: *const c_char,
    access: u32,
    _share: u32,
    _security: *mut c_void,
    disposition: u32,
    _flags: u32,
    _template: Handle,
) -> Handle {
    // SAFETY: guaranteed by the caller.
    let name = unsafe { text_a(name) };
    handle_or_invalid(create_file(name, access, disposition))
}

/// `CreateFileW`: see [`CreateFileA`].
///
/// # Safety
///
/// `name` must be null or a NUL-terminated UTF-16 string.
unsafe extern "win64" fn CreateFileW(
    name: *const u16,
    access: u32,
    _share: u32,
    _security: *mut c_void,
    disposition: u32,
    _flags: u32,
    _template: Handle,
) -> Handle {
    // SAFETY: guaranteed by the caller.
    let name = unsafe { text_w(name) };
    handle_or_invalid(create_file(
        name.as_deref().map_err(|&code| code),
        access,
        disposition,
    ))
}

/// `GetFileSizeEx`.
///
/// # Safety
///
/// `size` must be null or valid for a write.
unsafe extern "win64" fn GetFileSizeEx(handle: Handle, size: *mut i64) -> Bool {
    let path = with_state(|state| match state.handles.get(handle) {
        Some(Object::File { path, .. }) => Ok(path.clone()),
        Some(_) => Err(ERROR_INVALID_PARAMETER),
        None => Err(ERROR_INVALID_HANDLE),
    });
    let stat = match path.map(|path| omit_rt::stat(&path)) {
        Ok(Ok(stat)) => stat,
        Ok(Err(errno)) => return fail_errno(errno),
        Err(code) => return fail(code),
    };
    if size.is_null() {
        return fail(ERROR_NOACCESS);
    }
    // SAFETY: guaranteed by the caller.
    unsafe { size.write_unaligned(stat.size as i64) };
    TRUE
}

const FILE_TYPE_DISK: u32 = 1;
const FILE_TYPE_CHAR: u32 = 2;

/// `GetFileType`.
extern "win64" fn GetFileType(handle: Handle) -> u32 {
    with_state(|state| match state.handles.get(handle) {
        Some(Object::File { .. }) => {
            set_last_error(ERROR_SUCCESS);
            FILE_TYPE_DISK
        }
        Some(_) => {
            set_last_error(ERROR_SUCCESS);
            FILE_TYPE_CHAR
        }
        None => {
            set_last_error(ERROR_INVALID_HANDLE);
            0
        }
    })
}

/// `GetConsoleMode`: succeeds only for console handles, which is how
/// programs find out whether they are talking to a console. Input is
/// line-edited and echoed; output is processed and wraps.
///
/// # Safety
///
/// `mode` must be null or valid for a write.
unsafe extern "win64" fn GetConsoleMode(handle: Handle, mode: *mut u32) -> Bool {
    const INPUT_MODE: u32 = 0x1 | 0x2 | 0x4;
    const OUTPUT_MODE: u32 = 0x1 | 0x2;
    let value = with_state(|state| match state.handles.get(handle) {
        Some(Object::ConsoleInput) => Some(INPUT_MODE),
        Some(Object::ConsoleOutput(_)) => Some(OUTPUT_MODE),
        _ => None,
    });
    match value {
        Some(_) if mode.is_null() => fail(ERROR_NOACCESS),
        Some(value) => {
            // SAFETY: guaranteed by the caller.
            unsafe { mode.write_unaligned(value) };
            TRUE
        }
        None => fail(ERROR_INVALID_HANDLE),
    }
}

/// `GetACP`: text is UTF-8.
extern "win64" fn GetACP() -> u32 {
    CP_UTF8
}

/// `GetConsoleCP`.
extern "win64" fn GetConsoleCP() -> u32 {
    CP_UTF8
}

/// `GetConsoleOutputCP`.
extern "win64" fn GetConsoleOutputCP() -> u32 {
    CP_UTF8
}

fn result_to_bool(result: Result<(), u32>) -> Bool {
    match result {
        Ok(()) => TRUE,
        Err(code) => fail(code),
    }
}

fn is_directory(path: &str) -> Result<bool, u32> {
    omit_rt::stat(path)
        .map(|stat| stat.kind == KIND_DIRECTORY)
        .map_err(error::from_errno)
}

/// `DeleteFileA`. Directories are refused, as on Windows.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe extern "win64" fn DeleteFileA(name: *const c_char) -> Bool {
    // SAFETY: guaranteed by the caller.
    let name = unsafe { text_a(name) };
    result_to_bool(file_path(name).and_then(|path| {
        if is_directory(&path)? {
            return Err(ERROR_ACCESS_DENIED);
        }
        omit_rt::remove(&path).map_err(error::from_errno)
    }))
}

/// `CreateDirectoryA`. Security attributes are ignored.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe extern "win64" fn CreateDirectoryA(name: *const c_char, _security: *mut c_void) -> Bool {
    // SAFETY: guaranteed by the caller.
    let name = unsafe { text_a(name) };
    result_to_bool(
        file_path(name).and_then(|path| omit_rt::mkdir(&path).map_err(error::from_errno)),
    )
}

/// `RemoveDirectoryA`: removes an empty directory.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe extern "win64" fn RemoveDirectoryA(name: *const c_char) -> Bool {
    // SAFETY: guaranteed by the caller.
    let name = unsafe { text_a(name) };
    result_to_bool(file_path(name).and_then(|path| {
        if !is_directory(&path)? {
            return Err(ERROR_DIRECTORY);
        }
        omit_rt::remove(&path).map_err(error::from_errno)
    }))
}

/// `MoveFileA`: renames a file or directory; fails if the new name
/// exists.
///
/// # Safety
///
/// Both names must be null or NUL-terminated strings.
unsafe extern "win64" fn MoveFileA(from: *const c_char, to: *const c_char) -> Bool {
    // SAFETY: guaranteed by the caller.
    let (from, to) = unsafe { (text_a(from), text_a(to)) };
    result_to_bool(file_path(from).and_then(|from| {
        let to = file_path(to)?;
        if omit_rt::stat(&to).is_ok() {
            return Err(ERROR_ALREADY_EXISTS);
        }
        omit_rt::rename(&from, &to).map_err(error::from_errno)
    }))
}

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

/// `GetFileAttributesA`: reports whether the path is a directory or a
/// plain file; other attributes do not exist here.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe extern "win64" fn GetFileAttributesA(name: *const c_char) -> u32 {
    // SAFETY: guaranteed by the caller.
    let name = unsafe { text_a(name) };
    match file_path(name).and_then(|path| is_directory(&path)) {
        Ok(true) => FILE_ATTRIBUTE_DIRECTORY,
        Ok(false) => FILE_ATTRIBUTE_NORMAL,
        Err(code) => {
            set_last_error(code);
            INVALID_FILE_ATTRIBUTES
        }
    }
}

// ---------------------------------------------------------------------
// The export table
// ---------------------------------------------------------------------

macro_rules! exports {
    ($($name:ident),* $(,)?) => {
        /// Names of the implemented functions, in alphabetical order.
        pub const NAMES: &[&str] = &[$(stringify!($name)),*];

        fn lookup(name: &str) -> Option<usize> {
            match name {
                $(stringify!($name) => Some($name as *const () as usize),)*
                _ => None,
            }
        }
    };
}

exports! {
    CloseHandle,
    CreateDirectoryA,
    CreateFileA,
    CreateFileW,
    DeleteFileA,
    ExitProcess,
    FreeLibrary,
    GetACP,
    GetCommandLineA,
    GetCommandLineW,
    GetConsoleCP,
    GetConsoleMode,
    GetConsoleOutputCP,
    GetCurrentProcess,
    GetCurrentProcessId,
    GetCurrentThreadId,
    GetEnvironmentVariableA,
    GetFileAttributesA,
    GetFileSizeEx,
    GetFileType,
    GetLastError,
    GetModuleFileNameA,
    GetModuleHandleA,
    GetModuleHandleW,
    GetProcAddress,
    GetProcessHeap,
    GetStdHandle,
    GetTickCount,
    GetTickCount64,
    HeapAlloc,
    HeapFree,
    HeapReAlloc,
    HeapSize,
    IsDebuggerPresent,
    LoadLibraryA,
    MoveFileA,
    QueryPerformanceCounter,
    QueryPerformanceFrequency,
    ReadFile,
    RemoveDirectoryA,
    SetLastError,
    Sleep,
    VirtualAlloc,
    VirtualFree,
    WriteConsoleA,
    WriteConsoleW,
    WriteFile,
}

/// The address of the implementation of the `KERNEL32.dll` function
/// `symbol`, if there is one. Imports by ordinal are not supported: the
/// ordinals of system libraries are not a documented interface.
pub fn resolve(symbol: Symbol<'_>) -> Option<usize> {
    match symbol {
        Symbol::Name(name) => lookup(name),
        Symbol::Ordinal(_) => None,
    }
}
