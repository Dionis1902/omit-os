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
//! * File times are not recorded; listings report them as zero.
//! * Threads exist, but libraries are not told about them (no
//!   `DLL_THREAD_ATTACH`), threads cannot be created suspended, and a
//!   process that ends through its last thread reports exit code 0.
//! * The date and time are UTC: there are no time zones.
//! * There is no environment, and no way to tell whether standard output
//!   is the console or a file; it is reported as console.
//! * Console input ends lines with `\n`, not `\r\n`.

#![allow(
    non_snake_case,
    reason = "the functions carry the names Windows programs import"
)]

use alloc::alloc::{alloc, alloc_zeroed, dealloc, realloc};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::ffi::{CStr, c_char, c_void};
use core::sync::atomic::{AtomicU32, Ordering};

use omit_pe::Symbol;
use omit_rt::abi::{self, Errno, KIND_DIRECTORY, open, protect};
use omit_rt::sync::Mutex;
use omit_rt::thread::JoinHandle;
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
        readable: bool,
        writable: bool,
    },
    Thread {
        shared: Arc<ThreadShared>,
        handle: JoinHandle<()>,
    },
    Event(Arc<Event>),
    /// A directory listing in progress: the entries not yet handed out,
    /// last first.
    Find(Vec<FoundFile>),
    Process(Arc<Process>),
}

/// What a thread and the holders of its handle share.
struct ThreadShared {
    id: u32,
    /// `STILL_ACTIVE` until the thread ends.
    exit_code: AtomicU32,
}

/// `STILL_ACTIVE`: the "exit code" of a thread that has not ended.
const STILL_ACTIVE: u32 = 259;

/// An event object: a flag threads can wait for.
struct Event {
    /// 1 while signalled. Also the futex the waiters sleep on.
    signalled: AtomicU32,
    /// Manual-reset events stay signalled until reset and release every
    /// waiter; auto-reset events release one waiter and reset themselves.
    manual: bool,
}

impl Event {
    /// Waits until the event is signalled (consuming the signal if it is
    /// an auto-reset event) or the time has passed.
    fn wait(&self, timeout_ms: Option<u64>) -> bool {
        let deadline = timeout_ms.map(|timeout| omit_rt::uptime_ms().saturating_add(timeout));
        loop {
            let acquired = if self.manual {
                self.signalled.load(Ordering::Acquire) == 1
            } else {
                self.signalled
                    .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            };
            if acquired {
                return true;
            }
            let remaining = match deadline {
                Some(deadline) => {
                    let now = omit_rt::uptime_ms();
                    if now >= deadline {
                        return false;
                    }
                    Some(deadline - now)
                }
                None => None,
            };
            // Sleeps only while the event is still unsignalled.
            let _ = omit_rt::futex_wait(&self.signalled, 0, remaining);
        }
    }

    fn set(&self) {
        self.signalled.store(1, Ordering::Release);
        omit_rt::futex_wake(&self.signalled, if self.manual { usize::MAX } else { 1 });
    }
}

struct State {
    handles: Table<Object>,
    standard: [Handle; 3],
    image_base: usize,
    command_line: Vec<u8>,
    command_line_wide: Vec<u16>,
    /// Regions handed out by `VirtualAlloc`: base and size.
    regions: Vec<(usize, usize)>,
    /// Address of the process environment block.
    peb: usize,
    /// The thread environment block of every thread ever started (they
    /// are not released), for clearing thread-local storage slots.
    tebs: Vec<usize>,
    /// Bit `n` set: thread-local storage index `n` is allocated.
    tls_used: u64,
    /// Stack size for threads that do not ask for one.
    default_stack: usize,
}

/// Process-wide state, shared by all threads of the Windows program. The
/// lock is never held while code of the program or of a library runs.
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut state = STATE.lock();
    f(state.as_mut().expect("kernel32::init has run"))
}

/// What [`init`] needs to know about the loaded program.
pub struct Setup<'a> {
    pub image_base: usize,
    pub stack_top: usize,
    pub stack_limit: usize,
    /// Stack size the program asked for; also used for its threads.
    pub stack_size: usize,
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
/// The 64 thread-local storage slots.
const TEB_TLS_SLOTS: usize = 0x1480;
const TLS_SLOT_COUNT: u32 = 64;
/// Beyond the end of the real structure, inside the two pages allocated
/// for it: this implementation's own per-thread pointer (see
/// [`ThreadShared`]).
const TEB_PRIVATE_THREAD: usize = 0x1F00;
const TEB_SIZE: usize = 2 * PAGE;
const PEB_IMAGE_BASE: usize = 0x10;

/// Allocates and fills in a thread environment block. The caller makes it
/// the thread's own with `set_thread_pointer`.
fn create_teb(
    peb: usize,
    stack_top: usize,
    stack_limit: usize,
    thread_id: usize,
) -> Result<usize, Errno> {
    let teb = omit_rt::map(TEB_SIZE, PAGE)? as usize;
    let fields = [
        (TEB_STACK_BASE, stack_top),
        (TEB_STACK_LIMIT, stack_limit),
        (TEB_SELF, teb),
        (TEB_PROCESS_ID, omit_rt::getpid() as usize),
        (TEB_THREAD_ID, thread_id),
        (TEB_PEB, peb),
    ];
    for (offset, value) in fields {
        // SAFETY: every offset lies inside the pages just mapped, which
        // are writable and used by nothing else, and is 8-byte aligned.
        unsafe { ((teb + offset) as *mut usize).write(value) };
    }
    Ok(teb)
}

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

    let peb = omit_rt::map(PAGE, PAGE)? as usize;
    // SAFETY: inside the page just mapped, 8-byte aligned.
    unsafe { ((peb + PEB_IMAGE_BASE) as *mut usize).write(setup.image_base) };
    // The first thread's ID is the process ID, as tools expect.
    let teb = create_teb(
        peb,
        setup.stack_top,
        setup.stack_limit,
        omit_rt::getpid() as usize,
    )?;
    omit_rt::set_thread_pointer(teb)?;

    *STATE.lock() = Some(State {
        handles,
        standard,
        image_base: setup.image_base,
        command_line,
        command_line_wide,
        regions: Vec::new(),
        peb,
        tebs: alloc::vec![teb],
        tls_used: 0,
        default_stack: setup.stack_size,
    });
    Ok(())
}

/// The calling thread's environment block: Windows code finds it through
/// `GS`, and so does this.
fn teb() -> usize {
    let teb: usize;
    // SAFETY: every thread that runs Windows code has had its `GS` base
    // set to a block whose field at 0x30 holds the block's own address
    // (`create_teb`); the instruction only reads that field.
    unsafe {
        core::arch::asm!(
            "mov {}, gs:[0x30]",
            out(reg) teb,
            options(nostack, readonly, preserves_flags),
        );
    }
    teb
}

fn last_error_slot() -> *mut u32 {
    (teb() + TEB_LAST_ERROR) as *mut u32
}

pub(crate) fn set_last_error(code: u32) {
    // SAFETY: the TEB stays mapped for the life of the process; the field
    // is 4-byte aligned. Windows keeps the last error in the same place,
    // per thread.
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

/// `GetCurrentThreadId`.
extern "win64" fn GetCurrentThreadId() -> u32 {
    // SAFETY: the field was written by `create_teb` and never changes.
    unsafe { ((teb() + TEB_THREAD_ID) as *const usize).read() as u32 }
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

/// 100-nanosecond intervals between 1601-01-01, where Windows file
/// times start, and 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// `SYSTEMTIME`.
#[repr(C)]
pub struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

/// `GetSystemTimeAsFileTime`: the date and time (UTC) as a `FILETIME`.
/// On a machine without a usable clock the result is 0, the start of
/// 1601 — a date no program will mistake for the present.
unsafe extern "win64" fn GetSystemTimeAsFileTime(time: *mut u64) {
    if time.is_null() {
        return;
    }
    let value = omit_rt::realtime_us().map_or(0, |micros| micros * 10 + FILETIME_UNIX_EPOCH);
    // SAFETY: the program passes a pointer to its `FILETIME`, which is
    // two 32-bit halves, low first, and need not be aligned to 8 bytes.
    unsafe { time.write_unaligned(value) };
}

/// `GetSystemTime`: the date and time (UTC), broken down. Without a
/// usable clock: 1601-01-01 00:00:00, as for
/// [`GetSystemTimeAsFileTime`].
unsafe extern "win64" fn GetSystemTime(time: *mut SystemTime) {
    use abi::datetime::DateTime;
    if time.is_null() {
        return;
    }
    let (moment, milliseconds) = omit_rt::realtime_us()
        .and_then(|micros| {
            let moment = DateTime::from_unix((micros / 1_000_000) as i64)?;
            Some((moment, (micros / 1000 % 1000) as u16))
        })
        .unwrap_or((
            DateTime {
                year: 1601,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            },
            0,
        ));
    // SAFETY: the program passes a pointer to its `SYSTEMTIME`.
    unsafe {
        time.write_unaligned(SystemTime {
            year: moment.year.clamp(1601, 30827) as u16,
            month: u16::from(moment.month),
            day_of_week: u16::from(moment.weekday()),
            day: u16::from(moment.day),
            hour: u16::from(moment.hour),
            minute: u16::from(moment.minute),
            second: u16::from(moment.second),
            milliseconds,
        });
    }
}

/// `GetLocalTime`: the same as [`GetSystemTime`] — there are no time
/// zones, local time is UTC.
unsafe extern "win64" fn GetLocalTime(time: *mut SystemTime) {
    // SAFETY: forwarded.
    unsafe { GetSystemTime(time) }
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
        Some(Object::Thread { handle, .. }) => {
            // The thread itself is not affected. If it has ended, its
            // stack can go; otherwise it keeps it.
            handle.reclaim();
            TRUE
        }
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
            fd, readable: true, ..
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
                let access = if reading {
                    open::READ_WRITE
                } else {
                    open::WRITE
                };
                omit_rt::open(&path, access | change | mode)
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
            Object::File {
                fd,
                path,
                readable: reading || !writable,
                writable,
            }
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
/// ignored.
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
        Some(Object::ConsoleInput | Object::ConsoleOutput(_) | Object::Null) => {
            set_last_error(ERROR_SUCCESS);
            FILE_TYPE_CHAR
        }
        // Not a file of any kind.
        Some(Object::Thread { .. } | Object::Event(_) | Object::Find(_) | Object::Process(_)) => {
            set_last_error(ERROR_INVALID_HANDLE);
            0
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
// File positions
// ---------------------------------------------------------------------

/// Moves a file's position; `method` is `FILE_BEGIN` (0), `FILE_CURRENT`
/// (1) or `FILE_END` (2), which are the kernel's own values.
fn set_file_pointer(handle: Handle, distance: i64, method: u32) -> Result<u64, u32> {
    let fd = with_state(|state| match state.handles.get(handle) {
        Some(Object::File { fd, .. }) => Ok(*fd),
        Some(_) => Err(ERROR_INVALID_PARAMETER),
        None => Err(ERROR_INVALID_HANDLE),
    })?;
    if method > 2 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    omit_rt::seek(fd, distance, u64::from(method)).map_err(|errno| match errno {
        // A position before the start of the file.
        Errno::InvalidArgument => ERROR_NEGATIVE_SEEK,
        errno => error::from_errno(errno),
    })
}

/// `ERROR_NEGATIVE_SEEK`.
const ERROR_NEGATIVE_SEEK: u32 = 131;

/// `SetFilePointerEx`.
///
/// # Safety
///
/// `new_position` must be null or valid for a write.
unsafe extern "win64" fn SetFilePointerEx(
    handle: Handle,
    distance: i64,
    new_position: *mut i64,
    method: u32,
) -> Bool {
    match set_file_pointer(handle, distance, method) {
        Ok(position) => {
            if !new_position.is_null() {
                // SAFETY: guaranteed by the caller.
                unsafe { new_position.write_unaligned(position as i64) };
            }
            TRUE
        }
        Err(code) => fail(code),
    }
}

/// `SetFilePointer`: the older form, with the distance in two halves.
/// Returns the low half of the new position, or `INVALID_SET_FILE_POINTER`
/// (all ones) with the last error set — which is cleared on success so
/// that a genuine position of that value can be told apart.
///
/// # Safety
///
/// `distance_high` must be null or valid for a read and a write.
unsafe extern "win64" fn SetFilePointer(
    handle: Handle,
    distance_low: i32,
    distance_high: *mut i32,
    method: u32,
) -> u32 {
    let distance = if distance_high.is_null() {
        i64::from(distance_low)
    } else {
        // SAFETY: guaranteed by the caller.
        (i64::from(unsafe { distance_high.read_unaligned() }) << 32)
            | i64::from(distance_low as u32)
    };
    match set_file_pointer(handle, distance, method) {
        Ok(position) => {
            if distance_high.is_null() && position > u64::from(u32::MAX - 1) {
                // Does not fit in the one half the caller can receive.
                set_last_error(ERROR_INVALID_PARAMETER);
                return u32::MAX;
            }
            if !distance_high.is_null() {
                // SAFETY: guaranteed by the caller.
                unsafe { distance_high.write_unaligned((position >> 32) as i32) };
            }
            set_last_error(ERROR_SUCCESS);
            position as u32
        }
        Err(code) => {
            set_last_error(code);
            u32::MAX
        }
    }
}

// ---------------------------------------------------------------------
// Directory listings
// ---------------------------------------------------------------------

/// `ERROR_NO_MORE_FILES`.
const ERROR_NO_MORE_FILES: u32 = 18;
/// Size of `WIN32_FIND_DATAA`.
const FIND_DATA_SIZE: usize = 320;
/// Offset and size of its `cFileName` field.
const FIND_DATA_NAME: usize = 44;
const FIND_DATA_NAME_SIZE: usize = 260;

/// One entry of a directory listing in progress.
struct FoundFile {
    name: String,
    directory: bool,
    size: u64,
}

/// Fills a `WIN32_FIND_DATAA`. File times are zero: the file systems do
/// not record them.
///
/// # Safety
///
/// `data` must be valid for writing [`FIND_DATA_SIZE`] bytes.
unsafe fn write_find_data(data: *mut u8, file: &FoundFile) {
    let mut bytes = [0u8; FIND_DATA_SIZE];
    let attributes = if file.directory {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    bytes[..4].copy_from_slice(&attributes.to_le_bytes());
    // After three 8-byte times: the size, high half first.
    bytes[28..32].copy_from_slice(&((file.size >> 32) as u32).to_le_bytes());
    bytes[32..36].copy_from_slice(&(file.size as u32).to_le_bytes());
    // The name, cut to what fits with its terminator.
    let name = file.name.as_bytes();
    let length = name.len().min(FIND_DATA_NAME_SIZE - 1);
    bytes[FIND_DATA_NAME..FIND_DATA_NAME + length].copy_from_slice(&name[..length]);
    // SAFETY: guaranteed by the caller.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), data, FIND_DATA_SIZE) };
}

/// `FindFirstFileA`: starts listing the files that match a name with
/// wildcards (see `omit_win32::pattern`) in one directory. The entries
/// `.` and `..` are not reported; file times are zero; there are no
/// short names. Fails with `ERROR_FILE_NOT_FOUND` if nothing matches and
/// `ERROR_PATH_NOT_FOUND` if the directory does not exist.
///
/// # Safety
///
/// `pattern` must be null or a NUL-terminated string; `data` must be
/// null or valid for writing a `WIN32_FIND_DATAA`.
unsafe extern "win64" fn FindFirstFileA(pattern: *const c_char, data: *mut u8) -> Handle {
    // SAFETY: guaranteed by the caller.
    let pattern = unsafe { text_a(pattern) };
    let found = (|| {
        let pattern = pattern?;
        if data.is_null() {
            return Err(ERROR_INVALID_PARAMETER);
        }
        // The directory is everything up to the last separator.
        let (directory, name) = match pattern.rfind(['\\', '/']) {
            Some(at) => (&pattern[..=at], &pattern[at + 1..]),
            None => (".", pattern),
        };
        if name.is_empty() || omit_win32::pattern::has_wildcards(directory) {
            return Err(ERROR_INVALID_PARAMETER);
        }
        let directory = file_path(Ok(directory))?;
        let entries = omit_rt::read_dir(&directory).map_err(|errno| match errno {
            Errno::NotFound => ERROR_PATH_NOT_FOUND,
            errno => error::from_errno(errno),
        })?;
        let mut files: Vec<FoundFile> = entries
            .into_iter()
            .filter(|entry| omit_win32::pattern::matches(name, &entry.name))
            .map(|entry| {
                let path = alloc::format!("{}/{}", directory.trim_end_matches('/'), entry.name);
                let stat = omit_rt::stat(&path).ok();
                FoundFile {
                    directory: stat.is_some_and(|stat| stat.kind == KIND_DIRECTORY),
                    size: stat.map_or(0, |stat| stat.size),
                    name: entry.name,
                }
            })
            .collect();
        files.sort_by(|a, b| a.name.cmp(&b.name));
        // The listing is handed out from the end, so keep it reversed.
        files.reverse();
        let first = files.pop().ok_or(ERROR_FILE_NOT_FOUND)?;
        // SAFETY: `data` is not null; valid by the caller's guarantee.
        unsafe { write_find_data(data, &first) };
        with_state(|state| state.handles.insert(Object::Find(files)))
            .map_err(|_| ERROR_TOO_MANY_OPEN_FILES)
    })();
    handle_or_invalid(found)
}

/// `FindNextFileA`: the next match, in name order. Fails with
/// `ERROR_NO_MORE_FILES` after the last.
///
/// # Safety
///
/// `data` must be null or valid for writing a `WIN32_FIND_DATAA`.
unsafe extern "win64" fn FindNextFileA(handle: Handle, data: *mut u8) -> Bool {
    if data.is_null() {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let next = with_state(|state| match state.handles.get_mut(handle) {
        Some(Object::Find(files)) => files.pop().ok_or(ERROR_NO_MORE_FILES),
        _ => Err(ERROR_INVALID_HANDLE),
    });
    match next {
        Ok(file) => {
            // SAFETY: `data` is not null; valid by the caller's guarantee.
            unsafe { write_find_data(data, &file) };
            TRUE
        }
        Err(code) => fail(code),
    }
}

/// `FindClose`.
extern "win64" fn FindClose(handle: Handle) -> Bool {
    with_state(|state| {
        if matches!(state.handles.get(handle), Some(Object::Find(_))) {
            state.handles.remove(handle);
            TRUE
        } else {
            fail(ERROR_INVALID_HANDLE)
        }
    })
}

// ---------------------------------------------------------------------
// Other processes
// ---------------------------------------------------------------------

/// A program started with `CreateProcessA`.
struct Process {
    pid: u64,
    /// The exit code once it is known; [`STILL_ACTIVE`] until then. The
    /// kernel hands out an exit status only once, so it is kept here.
    exit_code: AtomicU32,
    finished: AtomicU32,
}

impl Process {
    /// Asks the kernel, without waiting, whether the program has ended.
    fn poll(&self) -> bool {
        if self.finished.load(Ordering::Acquire) != 0 {
            return true;
        }
        match omit_rt::try_wait(self.pid) {
            Ok(None) => false,
            // Ended — or already collected by another thread of this
            // program in the meantime, which stored the code.
            Ok(Some(status)) => {
                self.exit_code.store(status as u32, Ordering::Release);
                self.finished.store(1, Ordering::Release);
                true
            }
            Err(_) => {
                self.finished.store(1, Ordering::Release);
                true
            }
        }
    }

    /// Waits until the program has ended or the time has passed.
    fn wait(&self, timeout_ms: Option<u64>) -> bool {
        let deadline = timeout_ms.map(|timeout| omit_rt::uptime_ms().saturating_add(timeout));
        loop {
            if self.poll() {
                return true;
            }
            if deadline.is_some_and(|deadline| omit_rt::uptime_ms() >= deadline) {
                return false;
            }
            // There is no way to sleep until a process ends *or* a time
            // passes, so look again every timer tick.
            omit_rt::sleep_ms(10);
        }
    }
}

/// `PROCESS_INFORMATION`.
#[repr(C)]
pub struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

/// Finds the program file `name` (a Windows path): as given, then with
/// `.exe` added, then both in `C:\bin`, which stands in for the Windows
/// search path.
fn find_program(name: &str) -> Result<String, u32> {
    let direct = file_path(Ok(name))?;
    let mut candidates = alloc::vec![direct.clone(), alloc::format!("{direct}.exe")];
    if !name.contains(['\\', '/', ':']) {
        candidates.push(alloc::format!("/bin/{name}"));
        candidates.push(alloc::format!("/bin/{name}.exe"));
    }
    candidates
        .into_iter()
        .find(|path| matches!(is_directory(path), Ok(false)))
        .ok_or(ERROR_FILE_NOT_FOUND)
}

/// `CreateProcessA`: starts a program — a Windows program, or a native
/// one — as a new process with the given command line, and the standard
/// input, output and error of this one. The program is named by
/// `application`, or else by the first word of `command_line`. Not
/// supported and ignored: security attributes, handle inheritance
/// choices, creation flags (a process cannot be started suspended), an
/// environment, a different working directory, and the start-up
/// information. The "thread handle" returned is a second handle to the
/// process.
///
/// # Safety
///
/// `application` and `command_line` must each be null or a
/// NUL-terminated string; `information` must be null or valid for
/// writing a `PROCESS_INFORMATION`.
#[allow(
    clippy::too_many_arguments,
    reason = "the signature is that of the Windows function"
)]
unsafe extern "win64" fn CreateProcessA(
    application: *const c_char,
    command_line: *mut c_char,
    _process_attributes: *mut c_void,
    _thread_attributes: *mut c_void,
    _inherit_handles: Bool,
    _flags: u32,
    _environment: *mut c_void,
    _directory: *const c_char,
    _startup: *mut c_void,
    information: *mut ProcessInformation,
) -> Bool {
    let started = (|| {
        if information.is_null() {
            return Err(ERROR_INVALID_PARAMETER);
        }
        let command_line = if command_line.is_null() {
            None
        } else {
            // SAFETY: guaranteed by the caller.
            Some(unsafe { text_a(command_line) }?)
        };
        let mut arguments = command_line.map(cmdline::split).unwrap_or_default();
        let program = if application.is_null() {
            arguments.first().cloned().ok_or(ERROR_INVALID_PARAMETER)?
        } else {
            // SAFETY: guaranteed by the caller.
            String::from(unsafe { text_a(application) }?)
        };
        if arguments.is_empty() {
            arguments.push(program.clone());
        }
        let path = find_program(&program)?;
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let pid = omit_rt::spawn(&path, &arguments).map_err(error::from_errno)?;
        let process = Arc::new(Process {
            pid,
            exit_code: AtomicU32::new(STILL_ACTIVE),
            finished: AtomicU32::new(0),
        });
        let handles = with_state(|state| {
            let first = state
                .handles
                .insert(Object::Process(process.clone()))
                .ok()?;
            match state.handles.insert(Object::Process(process.clone())) {
                Ok(second) => Some((first, second)),
                Err(_) => {
                    state.handles.remove(first);
                    None
                }
            }
        });
        // (Without handles the program runs on, unobserved.)
        let (process_handle, thread_handle) = handles.ok_or(ERROR_TOO_MANY_OPEN_FILES)?;
        // SAFETY: `information` is not null; valid by the caller's
        // guarantee.
        unsafe {
            information.write_unaligned(ProcessInformation {
                process: process_handle,
                thread: thread_handle,
                process_id: pid as u32,
                thread_id: pid as u32,
            });
        }
        Ok(())
    })();
    match started {
        Ok(()) => TRUE,
        Err(code) => fail(code),
    }
}

/// `GetExitCodeProcess`: the exit code of a program started with
/// `CreateProcessA`, or `STILL_ACTIVE` while it runs.
///
/// # Safety
///
/// `code` must be null or valid for a write.
unsafe extern "win64" fn GetExitCodeProcess(handle: Handle, code: *mut u32) -> Bool {
    let process = with_state(|state| match state.handles.get(handle) {
        Some(Object::Process(process)) => Some(process.clone()),
        _ => None,
    });
    let Some(process) = process else {
        return fail(ERROR_INVALID_HANDLE);
    };
    if code.is_null() {
        return fail(ERROR_NOACCESS);
    }
    process.poll();
    // SAFETY: guaranteed by the caller.
    unsafe { code.write_unaligned(process.exit_code.load(Ordering::Acquire)) };
    TRUE
}

/// `TerminateProcess`: ends a program started with `CreateProcessA`, or
/// — given the current-process pseudo-handle — this one. A terminated
/// child reports the kernel's exit status for a killed process, not
/// `exit_code`: the kernel has no way to choose it.
extern "win64" fn TerminateProcess(handle: Handle, exit_code: u32) -> Bool {
    if handle == INVALID_HANDLE {
        omit_rt::exit(exit_code as i32);
    }
    let process = with_state(|state| match state.handles.get(handle) {
        Some(Object::Process(process)) => Some(process.clone()),
        _ => None,
    });
    match process {
        Some(process) if process.poll() => fail(ERROR_ACCESS_DENIED),
        Some(process) => match omit_rt::kill(process.pid) {
            Ok(()) => TRUE,
            Err(errno) => fail_errno(errno),
        },
        None => fail(ERROR_INVALID_HANDLE),
    }
}

// ---------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------

/// `INFINITE`: no timeout.
const INFINITE: u32 = u32::MAX;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const WAIT_FAILED: u32 = u32::MAX;

/// Source of thread IDs for threads other than the first.
static NEXT_THREAD_ID: AtomicU32 = AtomicU32::new(0x1000);

/// The calling thread's shared record, if it was created by
/// `CreateThread` (the program's first thread has none).
fn current_thread_shared() -> Option<&'static ThreadShared> {
    // SAFETY: the slot is inside the thread's own TEB and holds either
    // zero or a pointer from `Arc::into_raw` that is never released, so
    // the record lives as long as the process.
    unsafe {
        let pointer = ((teb() + TEB_PRIVATE_THREAD) as *const *const ThreadShared).read();
        pointer.as_ref()
    }
}

/// `CreateThread`: starts a thread at `start` with `parameter`. The stack
/// size is honoured within 64 KiB – 8 MiB (zero means the program's own
/// default). Creation flags are not supported: a thread always starts
/// running. Security attributes are ignored.
///
/// # Safety
///
/// `start` must be a function with the thread start routine signature
/// that is safe to call with `parameter`; `id` must be null or valid for
/// a write.
unsafe extern "win64" fn CreateThread(
    _security: *mut c_void,
    stack_size: usize,
    start: usize,
    parameter: usize,
    flags: u32,
    id: *mut u32,
) -> Handle {
    let refuse = |code| {
        set_last_error(code);
        0
    };
    if start == 0 {
        return refuse(ERROR_INVALID_PARAMETER);
    }
    if flags != 0 {
        return refuse(ERROR_NOT_SUPPORTED);
    }
    let (peb, default_stack) = with_state(|state| (state.peb, state.default_stack));
    let stack_size = if stack_size == 0 {
        default_stack
    } else {
        stack_size
    }
    .clamp(64 * 1024, 8 * 1024 * 1024)
    .next_multiple_of(PAGE);

    // Everything the thread needs is prepared here, so that failures are
    // reported to the caller rather than discovered by the new thread.
    let Ok(stack) = omit_rt::map(stack_size + PAGE, PAGE) else {
        return refuse(ERROR_NOT_ENOUGH_MEMORY);
    };
    // SAFETY: just mapped and unused: the page below the stack is removed
    // so that an overflow faults.
    let _ = unsafe { omit_rt::unmap(stack, PAGE) };
    let stack_limit = stack as usize + PAGE;
    let stack_top = stack_limit + stack_size;
    let release_stack = || {
        // SAFETY: the stack was never used.
        let _ = unsafe { omit_rt::unmap(stack_limit as *mut u8, stack_size) };
    };

    let shared = Arc::new(ThreadShared {
        id: NEXT_THREAD_ID.fetch_add(4, Ordering::Relaxed),
        exit_code: AtomicU32::new(STILL_ACTIVE),
    });
    let Ok(teb) = create_teb(peb, stack_top, stack_limit, shared.id as usize) else {
        release_stack();
        return refuse(ERROR_NOT_ENOUGH_MEMORY);
    };
    // The thread finds its record through its TEB (for `ExitThread`).
    // This reference is never released, like the TEB itself.
    // SAFETY: inside the TEB just mapped, 8-byte aligned.
    unsafe {
        ((teb + TEB_PRIVATE_THREAD) as *mut *const ThreadShared)
            .write(Arc::into_raw(shared.clone()));
    }
    with_state(|state| state.tebs.push(teb));

    let in_thread = shared.clone();
    let spawned = omit_rt::thread::spawn(move || {
        if omit_rt::set_thread_pointer(teb).is_err() {
            in_thread.exit_code.store(u32::MAX, Ordering::Release);
            return;
        }
        // SAFETY: the caller of `CreateThread` vouches for the start
        // routine; it runs on the stack prepared above, as Windows would
        // run it, with its parameter as the only argument.
        let code = unsafe { crate::call_on_stack(start, stack_top, [parameter, 0, 0]) };
        in_thread.exit_code.store(code as u32, Ordering::Release);
    });
    let handle = match spawned {
        Ok(handle) => handle,
        Err(_) => {
            release_stack();
            return refuse(ERROR_NOT_ENOUGH_MEMORY);
        }
    };
    let thread_id = shared.id;
    match with_state(|state| state.handles.insert(Object::Thread { shared, handle })) {
        Ok(handle) => {
            if !id.is_null() {
                // SAFETY: guaranteed by the caller.
                unsafe { id.write_unaligned(thread_id) };
            }
            handle
        }
        // The thread runs, but there is no handle to give out.
        Err(_) => refuse(ERROR_TOO_MANY_OPEN_FILES),
    }
}

/// `ExitThread`: ends the calling thread with the given exit code.
extern "win64" fn ExitThread(code: u32) -> ! {
    if let Some(shared) = current_thread_shared() {
        shared.exit_code.store(code, Ordering::Release);
    }
    omit_rt::thread::exit()
}

/// `GetExitCodeThread`: the thread's exit code, or `STILL_ACTIVE` (259)
/// while it runs.
///
/// # Safety
///
/// `code` must be null or valid for a write.
unsafe extern "win64" fn GetExitCodeThread(handle: Handle, code: *mut u32) -> Bool {
    let value = with_state(|state| match state.handles.get(handle) {
        Some(Object::Thread { shared, handle }) => {
            // The code is stored just before the thread ends; report it
            // only once the thread really has.
            Some(if handle.is_finished() {
                shared.exit_code.load(Ordering::Acquire)
            } else {
                STILL_ACTIVE
            })
        }
        _ => None,
    });
    match value {
        Some(_) if code.is_null() => fail(ERROR_NOACCESS),
        Some(value) => {
            // SAFETY: guaranteed by the caller.
            unsafe { code.write_unaligned(value) };
            TRUE
        }
        None => fail(ERROR_INVALID_HANDLE),
    }
}

/// `GetCurrentThread`: the pseudo-handle for the calling thread. It is
/// not accepted by the wait functions here.
extern "win64" fn GetCurrentThread() -> Handle {
    usize::MAX - 1
}

/// `SwitchToThread`: lets another thread run.
extern "win64" fn SwitchToThread() -> Bool {
    omit_rt::yield_now();
    TRUE
}

/// `WaitForSingleObject`: waits until a thread has ended or an event is
/// signalled, or until `milliseconds` have passed (`INFINITE`: no limit).
/// Returns `WAIT_OBJECT_0` (0), `WAIT_TIMEOUT` (258) or `WAIT_FAILED`.
extern "win64" fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32 {
    enum Target {
        Thread(omit_rt::thread::Waiter),
        Event(Arc<Event>),
        Process(Arc<Process>),
    }
    // The lock is released before waiting.
    let target = with_state(|state| match state.handles.get(handle) {
        Some(Object::Thread { handle, .. }) => Some(Target::Thread(handle.waiter())),
        Some(Object::Event(event)) => Some(Target::Event(event.clone())),
        Some(Object::Process(process)) => Some(Target::Process(process.clone())),
        _ => None,
    });
    let timeout = (milliseconds != INFINITE).then_some(u64::from(milliseconds));
    let signalled = match target {
        Some(Target::Thread(waiter)) => waiter.wait(timeout),
        Some(Target::Event(event)) => event.wait(timeout),
        Some(Target::Process(process)) => process.wait(timeout),
        None => {
            set_last_error(ERROR_INVALID_HANDLE);
            return WAIT_FAILED;
        }
    };
    if signalled {
        WAIT_OBJECT_0
    } else {
        WAIT_TIMEOUT
    }
}

// ---------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------

/// `CreateEventA`: creates an unnamed event. Named events (shared between
/// processes on Windows) are not supported.
///
/// # Safety
///
/// `name` is only compared with null.
unsafe extern "win64" fn CreateEventA(
    _security: *mut c_void,
    manual_reset: Bool,
    initial_state: Bool,
    name: *const c_char,
) -> Handle {
    if !name.is_null() {
        set_last_error(ERROR_NOT_SUPPORTED);
        return 0;
    }
    let event = Arc::new(Event {
        signalled: AtomicU32::new(u32::from(initial_state != FALSE)),
        manual: manual_reset != FALSE,
    });
    match with_state(|state| state.handles.insert(Object::Event(event))) {
        Ok(handle) => {
            set_last_error(ERROR_SUCCESS);
            handle
        }
        Err(_) => {
            set_last_error(ERROR_TOO_MANY_OPEN_FILES);
            0
        }
    }
}

fn with_event(handle: Handle, action: impl FnOnce(&Event)) -> Bool {
    let event = with_state(|state| match state.handles.get(handle) {
        Some(Object::Event(event)) => Some(event.clone()),
        _ => None,
    });
    match event {
        Some(event) => {
            action(&event);
            TRUE
        }
        None => fail(ERROR_INVALID_HANDLE),
    }
}

/// `SetEvent`: signals the event. A manual-reset event releases every
/// waiter and stays signalled; an auto-reset event releases one.
extern "win64" fn SetEvent(handle: Handle) -> Bool {
    with_event(handle, Event::set)
}

/// `ResetEvent`: makes the event unsignalled.
extern "win64" fn ResetEvent(handle: Handle) -> Bool {
    with_event(handle, |event| event.signalled.store(0, Ordering::Release))
}

// ---------------------------------------------------------------------
// Critical sections
// ---------------------------------------------------------------------

/// `CRITICAL_SECTION`. The program provides the memory (40 bytes) and
/// must treat it as opaque; the fields have the documented names and
/// sizes, and this implementation gives them its own meaning: `lock` is a
/// futex word (0 free, 1 held, 2 held with waiters), `owner` the ID of
/// the holding thread, `recursion` how many times it entered.
#[repr(C)]
struct CriticalSection {
    debug_info: usize,
    lock: AtomicU32,
    recursion: u32,
    owner: usize,
    semaphore: usize,
    spin_count: usize,
}

const _: () = assert!(size_of::<CriticalSection>() == 40);

/// `InitializeCriticalSection`.
///
/// # Safety
///
/// `section` must point to 40 writable bytes that no thread is using as a
/// critical section.
unsafe extern "win64" fn InitializeCriticalSection(section: *mut CriticalSection) {
    // SAFETY: guaranteed by the caller.
    unsafe { section.write_bytes(0, 1) };
}

/// `InitializeCriticalSectionAndSpinCount`: the spin count only matters
/// with several processors and is ignored.
///
/// # Safety
///
/// As for [`InitializeCriticalSection`].
unsafe extern "win64" fn InitializeCriticalSectionAndSpinCount(
    section: *mut CriticalSection,
    _spin_count: u32,
) -> Bool {
    // SAFETY: same contract.
    unsafe { InitializeCriticalSection(section) };
    TRUE
}

/// `DeleteCriticalSection`.
///
/// # Safety
///
/// `section` must be an initialised critical section that no thread
/// holds or waits for.
unsafe extern "win64" fn DeleteCriticalSection(section: *mut CriticalSection) {
    // SAFETY: guaranteed by the caller.
    unsafe { section.write_bytes(0, 1) };
}

/// Takes the section for the calling thread, which does not hold it.
///
/// # Safety
///
/// `section` must be an initialised critical section.
unsafe fn take_section(section: *mut CriticalSection, me: usize, wait: bool) -> bool {
    // SAFETY: the lock word is an atomic shared by all threads; the other
    // fields are only written by the thread that holds the lock, which
    // from here on is this one.
    unsafe {
        let lock = &(*section).lock;
        if lock
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if !wait {
                return false;
            }
            // Same protocol as the runtime's mutex.
            while lock.swap(2, Ordering::Acquire) != 0 {
                let _ = omit_rt::futex_wait(lock, 2, None);
            }
        }
        (&raw mut (*section).owner).write(me);
        (&raw mut (*section).recursion).write(1);
    }
    true
}

/// Whether the calling thread (`me`) holds the section; if so, counts
/// another entry.
///
/// # Safety
///
/// `section` must be an initialised critical section.
unsafe fn reenter_section(section: *mut CriticalSection, me: usize) -> bool {
    // SAFETY: `owner` is written only by the holder: reading it races
    // with other threads, but it can only equal this thread's ID if this
    // thread wrote it, in which case nobody else writes these fields.
    unsafe {
        if (&raw const (*section).owner).read_volatile() != me {
            return false;
        }
        (*section).recursion += 1;
    }
    true
}

/// `EnterCriticalSection`: waits until the calling thread owns the
/// section. A thread may enter a section it already owns.
///
/// # Safety
///
/// `section` must be an initialised critical section.
unsafe extern "win64" fn EnterCriticalSection(section: *mut CriticalSection) {
    let me = GetCurrentThreadId() as usize;
    // SAFETY: guaranteed by the caller.
    unsafe {
        if !reenter_section(section, me) {
            take_section(section, me, true);
        }
    }
}

/// `TryEnterCriticalSection`: like [`EnterCriticalSection`] without
/// waiting; returns whether the thread now owns the section.
///
/// # Safety
///
/// `section` must be an initialised critical section.
unsafe extern "win64" fn TryEnterCriticalSection(section: *mut CriticalSection) -> Bool {
    let me = GetCurrentThreadId() as usize;
    // SAFETY: guaranteed by the caller.
    let entered = unsafe { reenter_section(section, me) || take_section(section, me, false) };
    Bool::from(entered)
}

/// `LeaveCriticalSection`: undoes one entry; the section becomes free
/// when the owner has left as often as it entered.
///
/// # Safety
///
/// `section` must be a critical section the calling thread owns.
unsafe extern "win64" fn LeaveCriticalSection(section: *mut CriticalSection) {
    // SAFETY: the caller owns the section, so it alone writes the fields.
    unsafe {
        (*section).recursion = (*section).recursion.saturating_sub(1);
        if (*section).recursion == 0 {
            (&raw mut (*section).owner).write(0);
            let lock = &(*section).lock;
            if lock.swap(0, Ordering::Release) == 2 {
                omit_rt::futex_wake(lock, 1);
            }
        }
    }
}

// ---------------------------------------------------------------------
// Thread-local storage
// ---------------------------------------------------------------------

const TLS_OUT_OF_INDEXES: u32 = u32::MAX;

/// `TlsAlloc`: reserves an index under which every thread can keep one
/// pointer-sized value, initially zero in all threads. Up to 64 indexes.
extern "win64" fn TlsAlloc() -> u32 {
    with_state(|state| {
        let index = (!state.tls_used).trailing_zeros();
        if index >= TLS_SLOT_COUNT {
            set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            return TLS_OUT_OF_INDEXES;
        }
        state.tls_used |= 1 << index;
        // A previous user of the index may have left values behind.
        for &teb in &state.tebs {
            // SAFETY: inside a TEB, which stays mapped; a thread writing
            // its own slot at the same moment would be using an index it
            // was never given.
            unsafe { ((teb + TEB_TLS_SLOTS + 8 * index as usize) as *mut usize).write(0) };
        }
        index
    })
}

/// `TlsFree`.
extern "win64" fn TlsFree(index: u32) -> Bool {
    with_state(|state| {
        if index < TLS_SLOT_COUNT && state.tls_used & (1 << index) != 0 {
            state.tls_used &= !(1 << index);
            TRUE
        } else {
            fail(ERROR_INVALID_PARAMETER)
        }
    })
}

/// `TlsGetValue`: the calling thread's value for the index. Succeeding
/// sets the last error to zero, so that a stored zero can be told from
/// failure.
extern "win64" fn TlsGetValue(index: u32) -> usize {
    if index >= TLS_SLOT_COUNT {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    set_last_error(ERROR_SUCCESS);
    // SAFETY: inside the calling thread's own TEB.
    unsafe { ((teb() + TEB_TLS_SLOTS + 8 * index as usize) as *const usize).read() }
}

/// `TlsSetValue`: sets the calling thread's value for the index.
extern "win64" fn TlsSetValue(index: u32, value: usize) -> Bool {
    let allocated =
        index < TLS_SLOT_COUNT && with_state(|state| state.tls_used & (1 << index) != 0);
    if !allocated {
        return fail(ERROR_INVALID_PARAMETER);
    }
    // SAFETY: inside the calling thread's own TEB.
    unsafe { ((teb() + TEB_TLS_SLOTS + 8 * index as usize) as *mut usize).write(value) };
    TRUE
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
    CreateEventA,
    CreateFileA,
    CreateFileW,
    CreateProcessA,
    CreateThread,
    DeleteCriticalSection,
    DeleteFileA,
    EnterCriticalSection,
    ExitProcess,
    ExitThread,
    FindClose,
    FindFirstFileA,
    FindNextFileA,
    FreeLibrary,
    GetACP,
    GetCommandLineA,
    GetCommandLineW,
    GetConsoleCP,
    GetConsoleMode,
    GetConsoleOutputCP,
    GetCurrentProcess,
    GetCurrentProcessId,
    GetCurrentThread,
    GetCurrentThreadId,
    GetEnvironmentVariableA,
    GetExitCodeProcess,
    GetExitCodeThread,
    GetFileAttributesA,
    GetFileSizeEx,
    GetFileType,
    GetLastError,
    GetLocalTime,
    GetModuleFileNameA,
    GetModuleHandleA,
    GetModuleHandleW,
    GetProcAddress,
    GetProcessHeap,
    GetStdHandle,
    GetSystemTime,
    GetSystemTimeAsFileTime,
    GetTickCount,
    GetTickCount64,
    HeapAlloc,
    HeapFree,
    HeapReAlloc,
    HeapSize,
    InitializeCriticalSection,
    InitializeCriticalSectionAndSpinCount,
    IsDebuggerPresent,
    LeaveCriticalSection,
    LoadLibraryA,
    MoveFileA,
    QueryPerformanceCounter,
    QueryPerformanceFrequency,
    ReadFile,
    RemoveDirectoryA,
    ResetEvent,
    SetEvent,
    SetFilePointer,
    SetFilePointerEx,
    SetLastError,
    Sleep,
    SwitchToThread,
    TerminateProcess,
    TlsAlloc,
    TlsFree,
    TlsGetValue,
    TlsSetValue,
    TryEnterCriticalSection,
    VirtualAlloc,
    VirtualFree,
    WaitForSingleObject,
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
